// Copyright (C) 2026 The Argus Capture community
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use digest_auth::{AuthContext, parse as parse_digest_challenge};
use futures::stream::{self, StreamExt};
use if_addrs::{IfAddr, get_if_addrs};
use oxvif::{DiscoveredDevice, OnvifClient, discovery};
use reqwest::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::config::ConfiguredCamera;

const DISCOVERY_ROUNDS: usize = 3;
const MULTICAST_TIMEOUT: Duration = Duration::from_secs(1);
const ROUND_INTERVAL: Duration = Duration::from_millis(250);
const UNICAST_TIMEOUT: Duration = Duration::from_millis(500);
const DISCOVERY_CONCURRENCY: usize = 32;
const MAX_SCAN_PREFIX: u8 = 24;
const HTTP_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const UNKNOWN_CAMERA: &str = "Unknown ONVIF camera";
const CANON_CCAPI_CAMERA: &str = "CCAPI camera";

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct NetworkCamera {
    pub(crate) address: String,
    pub(crate) port: u16,
    pub(crate) manufacturer: Option<String>,
    pub(crate) model: String,
}

impl NetworkCamera {
    pub(crate) fn display_name(&self) -> String {
        match self.manufacturer.as_deref() {
            Some(manufacturer)
                if !manufacturer.eq_ignore_ascii_case(&self.model)
                    && !starts_with_ignore_ascii_case(&self.model, manufacturer) =>
            {
                format!("{manufacturer} {}", self.model)
            }
            _ => self.model.clone(),
        }
    }
}

pub(crate) async fn discover_network_cameras(
    configured_camera: Option<&ConfiguredCamera>,
) -> Vec<NetworkCamera> {
    let scan_targets = subnet_scan_targets().unwrap_or_default();
    let mut cameras = HashMap::new();

    for camera in discover_onvif_cameras(&scan_targets).await {
        merge_camera(&mut cameras, camera);
    }

    for camera in discover_canon_cameras(&scan_targets).await {
        merge_camera(&mut cameras, camera);
    }

    if let Some(configured_camera) = configured_camera
        && let Some(camera) = probe_configured_camera(configured_camera).await
    {
        merge_camera(&mut cameras, camera);
    }

    let mut cameras = cameras.into_values().collect::<Vec<_>>();
    cameras.sort_by(|left, right| {
        left.address
            .cmp(&right.address)
            .then_with(|| left.model.cmp(&right.model))
            .then_with(|| left.manufacturer.cmp(&right.manufacturer))
    });

    cameras
}

pub(crate) async fn inspect_configured_camera(
    configured_camera: &ConfiguredCamera,
) -> Option<NetworkCamera> {
    probe_configured_camera(configured_camera).await
}

async fn discover_onvif_cameras(scan_targets: &[Ipv4Addr]) -> Vec<NetworkCamera> {
    let mut devices = deduplicate_devices(
        discovery::probe_rounds(DISCOVERY_ROUNDS, MULTICAST_TIMEOUT, ROUND_INTERVAL).await,
    );

    if !scan_targets.is_empty() {
        let responses = stream::iter(scan_targets.iter().copied().map(|ip| async move {
            discovery::probe_unicast(IpAddr::V4(ip), UNICAST_TIMEOUT).await
        }))
        .buffer_unordered(DISCOVERY_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

        for response in responses {
            merge_devices(&mut devices, response);
        }
    }

    stream::iter(devices.into_values())
        .map(describe_device)
        .buffer_unordered(16)
        .filter_map(|camera| async move { camera })
        .collect::<Vec<_>>()
        .await
}

async fn discover_canon_cameras(scan_targets: &[Ipv4Addr]) -> Vec<NetworkCamera> {
    let Ok(client) = Client::builder()
        .timeout(HTTP_PROBE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
    else {
        return Vec::new();
    };

    stream::iter(scan_targets.iter().copied().map(|ip| {
        let client = client.clone();
        async move { probe_canon_camera(&client, ip).await }
    }))
    .buffer_unordered(DISCOVERY_CONCURRENCY)
    .filter_map(|camera| async move { camera })
    .collect::<Vec<_>>()
    .await
}

async fn describe_device(device: DiscoveredDevice) -> Option<NetworkCamera> {
    let xaddr = primary_xaddr(&device)?;
    let address = host_from_xaddr(&xaddr).unwrap_or_else(|| xaddr.clone());
    let port = port_from_xaddr(&xaddr).unwrap_or(80);
    let info = OnvifClient::new(&xaddr).get_device_info().await.ok();

    let manufacturer = info
        .as_ref()
        .and_then(|info| non_empty(&info.manufacturer))
        .map(str::to_owned);

    let model = info
        .as_ref()
        .and_then(|info| non_empty(&info.model))
        .map(str::to_owned)
        .or_else(|| scope_value(&device.scopes, "hardware"))
        .or_else(|| scope_value(&device.scopes, "name"))
        .unwrap_or_else(|| UNKNOWN_CAMERA.to_owned());

    Some(NetworkCamera {
        address,
        port,
        manufacturer,
        model,
    })
}

fn deduplicate_devices(devices: Vec<DiscoveredDevice>) -> HashMap<String, DiscoveredDevice> {
    let mut unique = HashMap::new();
    merge_devices(&mut unique, devices);
    unique
}

fn merge_devices(unique: &mut HashMap<String, DiscoveredDevice>, devices: Vec<DiscoveredDevice>) {
    for device in devices {
        let Some(key) = device_key(&device) else {
            continue;
        };

        match unique.get_mut(&key) {
            Some(existing) => merge_device_details(existing, device),
            None => {
                unique.insert(key, device);
            }
        }
    }
}

fn merge_device_details(existing: &mut DiscoveredDevice, incoming: DiscoveredDevice) {
    if existing.endpoint.trim().is_empty() {
        existing.endpoint = incoming.endpoint;
    }

    extend_unique(&mut existing.types, incoming.types);
    extend_unique(&mut existing.scopes, incoming.scopes);
    extend_unique(&mut existing.xaddrs, incoming.xaddrs);
}

fn extend_unique(target: &mut Vec<String>, additions: Vec<String>) {
    for addition in additions {
        if !target.contains(&addition) {
            target.push(addition);
        }
    }
}

fn device_key(device: &DiscoveredDevice) -> Option<String> {
    non_empty(&device.endpoint)
        .map(str::to_owned)
        .or_else(|| primary_xaddr(device))
}

fn primary_xaddr(device: &DiscoveredDevice) -> Option<String> {
    device
        .xaddrs
        .iter()
        .find(|xaddr| host_from_xaddr(xaddr).is_some())
        .cloned()
        .or_else(|| device.xaddrs.first().cloned())
}

fn host_from_xaddr(xaddr: &str) -> Option<String> {
    Url::parse(xaddr)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
}

fn port_from_xaddr(xaddr: &str) -> Option<u16> {
    Url::parse(xaddr)
        .ok()
        .and_then(|url| url.port_or_known_default())
}

fn scope_value(scopes: &[String], scope_kind: &str) -> Option<String> {
    let prefix = format!("onvif://www.onvif.org/{scope_kind}/");

    scopes
        .iter()
        .find_map(|scope| scope.strip_prefix(&prefix))
        .and_then(non_empty)
        .map(|value| value.replace("%20", " "))
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

async fn probe_canon_camera(client: &Client, ip: Ipv4Addr) -> Option<NetworkCamera> {
    let base_url = format!("http://{ip}");
    let inventory = client.get(format!("{base_url}/ccapi")).send().await.ok()?;

    if inventory.status() == StatusCode::UNAUTHORIZED {
        let challenge = inventory.headers().get(WWW_AUTHENTICATE)?.to_str().ok()?;
        if challenge.contains("CameraControlApi") {
            return Some(NetworkCamera {
                address: ip.to_string(),
                port: 80,
                manufacturer: Some("Canon".to_owned()),
                model: CANON_CCAPI_CAMERA.to_owned(),
            });
        }

        return None;
    }

    if !inventory.status().is_success() {
        return None;
    }

    let device_info = client
        .get(format!("{base_url}/ccapi/ver100/deviceinformation"))
        .send()
        .await
        .ok()?;

    if device_info.status() == StatusCode::UNAUTHORIZED {
        return Some(NetworkCamera {
            address: ip.to_string(),
            port: 80,
            manufacturer: Some("Canon".to_owned()),
            model: CANON_CCAPI_CAMERA.to_owned(),
        });
    }

    if !device_info.status().is_success() {
        return Some(NetworkCamera {
            address: ip.to_string(),
            port: 80,
            manufacturer: Some("Canon".to_owned()),
            model: CANON_CCAPI_CAMERA.to_owned(),
        });
    }

    let info = device_info.json::<CanonDeviceInformation>().await.ok()?;
    let manufacturer = info
        .manufacturer
        .as_deref()
        .and_then(normalize_manufacturer)
        .or_else(|| Some("Canon".to_owned()));
    let model = info
        .modeldescription
        .as_deref()
        .and_then(non_empty)
        .or_else(|| info.productname.as_deref().and_then(non_empty))
        .map(str::to_owned)
        .unwrap_or_else(|| CANON_CCAPI_CAMERA.to_owned());

    Some(NetworkCamera {
        address: ip.to_string(),
        port: 80,
        manufacturer,
        model,
    })
}

async fn probe_configured_camera(configured_camera: &ConfiguredCamera) -> Option<NetworkCamera> {
    let Ok(client) = Client::builder()
        .timeout(HTTP_PROBE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
    else {
        return None;
    };

    probe_configured_canon_camera(&client, configured_camera).await
}

async fn probe_configured_canon_camera(
    client: &Client,
    configured_camera: &ConfiguredCamera,
) -> Option<NetworkCamera> {
    let base_url = format!(
        "http://{}:{}",
        configured_camera.host, configured_camera.port
    );
    let inventory = client.get(format!("{base_url}/ccapi")).send().await.ok()?;

    let reachable = match inventory.status() {
        StatusCode::UNAUTHORIZED => inventory
            .headers()
            .get(WWW_AUTHENTICATE)
            .and_then(|header| header.to_str().ok())
            .is_some_and(|challenge| challenge.contains("CameraControlApi")),
        status => status.is_success(),
    };

    if !reachable {
        return None;
    }

    if let Some(info) = fetch_json_with_optional_digest::<CanonDeviceInformation>(
        client,
        &format!("{base_url}/ccapi/ver100/deviceinformation"),
        configured_camera.credentials(),
    )
    .await
    {
        let manufacturer = info
            .manufacturer
            .as_deref()
            .and_then(normalize_manufacturer);
        let model = info
            .modeldescription
            .as_deref()
            .and_then(non_empty)
            .or_else(|| info.productname.as_deref().and_then(non_empty))
            .map(str::to_owned)
            .unwrap_or_else(|| configured_camera.name.clone());

        return Some(NetworkCamera {
            address: configured_camera.host.clone(),
            port: configured_camera.port,
            manufacturer,
            model,
        });
    }

    Some(NetworkCamera {
        address: configured_camera.host.clone(),
        port: configured_camera.port,
        manufacturer: None,
        model: configured_camera.name.clone(),
    })
}

async fn fetch_json_with_optional_digest<T>(
    client: &Client,
    url: &str,
    credentials: Option<(&str, &str)>,
) -> Option<T>
where
    T: DeserializeOwned,
{
    let response = client.get(url).send().await.ok()?;

    if response.status().is_success() {
        return response.json::<T>().await.ok();
    }

    if response.status() != StatusCode::UNAUTHORIZED {
        return None;
    }

    let (username, password) = credentials?;
    let challenge = response.headers().get(WWW_AUTHENTICATE)?.to_str().ok()?;
    let authorization = digest_authorization_header(response.url(), challenge, username, password)?;

    let authenticated = client
        .get(url)
        .header(AUTHORIZATION, authorization)
        .send()
        .await
        .ok()?;

    authenticated.status().is_success().then_some(())?;
    authenticated.json::<T>().await.ok()
}

fn digest_authorization_header(
    url: &Url,
    challenge: &str,
    username: &str,
    password: &str,
) -> Option<String> {
    let mut prompt = parse_digest_challenge(challenge).ok()?;
    let uri = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    };
    let context = AuthContext::new(username, password, uri);

    prompt
        .respond(&context)
        .ok()
        .map(|header| header.to_string())
}

fn merge_camera(cameras: &mut HashMap<String, NetworkCamera>, incoming: NetworkCamera) {
    match cameras.get_mut(&incoming.address) {
        Some(existing) => {
            if existing.manufacturer.is_none() && incoming.manufacturer.is_some() {
                existing.manufacturer = incoming.manufacturer;
            }

            if model_is_placeholder(&existing.model) && !model_is_placeholder(&incoming.model) {
                existing.model = incoming.model;
            }
        }
        None => {
            cameras.insert(incoming.address.clone(), incoming);
        }
    }
}

fn model_is_placeholder(model: &str) -> bool {
    matches!(model, UNKNOWN_CAMERA | CANON_CCAPI_CAMERA)
}

fn normalize_manufacturer(value: &str) -> Option<String> {
    match non_empty(value) {
        Some("Canon Inc.") => Some("Canon".to_owned()),
        Some(other) => Some(other.to_owned()),
        None => None,
    }
}

fn subnet_scan_targets() -> io::Result<Vec<Ipv4Addr>> {
    let mut targets = BTreeSet::new();

    for interface in get_if_addrs()? {
        if !interface.is_oper_up() || interface.is_loopback() || interface.is_p2p() {
            continue;
        }

        let IfAddr::V4(address) = interface.addr else {
            continue;
        };

        if !address.ip.is_private() || address.ip.is_link_local() {
            continue;
        }

        targets.extend(hosts_in_scan_range(address.ip, address.prefixlen));
    }

    Ok(targets.into_iter().collect())
}

fn hosts_in_scan_range(ip: Ipv4Addr, prefixlen: u8) -> Vec<Ipv4Addr> {
    if prefixlen >= 31 {
        return Vec::new();
    }

    let effective_prefix = prefixlen.max(MAX_SCAN_PREFIX);
    let mask = u32::MAX << (32 - u32::from(effective_prefix));
    let ip_bits = u32::from(ip);
    let network = ip_bits & mask;
    let broadcast = network | !mask;

    if broadcast <= network + 1 {
        return Vec::new();
    }

    (network + 1..broadcast)
        .map(Ipv4Addr::from)
        .filter(|candidate| *candidate != ip)
        .collect()
}

#[derive(Debug, Deserialize)]
struct CanonDeviceInformation {
    manufacturer: Option<String>,
    modeldescription: Option<String>,
    productname: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_large_networks_to_the_local_slash_24() {
        let hosts = hosts_in_scan_range(Ipv4Addr::new(192, 168, 1, 23), 16);

        assert_eq!(hosts.len(), 253);
        assert_eq!(hosts.first(), Some(&Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(hosts.last(), Some(&Ipv4Addr::new(192, 168, 1, 254)));
        assert!(!hosts.contains(&Ipv4Addr::new(192, 168, 1, 23)));
    }

    #[test]
    fn respects_smaller_subnets() {
        let hosts = hosts_in_scan_range(Ipv4Addr::new(192, 168, 1, 42), 28);

        assert_eq!(
            hosts,
            vec![
                Ipv4Addr::new(192, 168, 1, 33),
                Ipv4Addr::new(192, 168, 1, 34),
                Ipv4Addr::new(192, 168, 1, 35),
                Ipv4Addr::new(192, 168, 1, 36),
                Ipv4Addr::new(192, 168, 1, 37),
                Ipv4Addr::new(192, 168, 1, 38),
                Ipv4Addr::new(192, 168, 1, 39),
                Ipv4Addr::new(192, 168, 1, 40),
                Ipv4Addr::new(192, 168, 1, 41),
                Ipv4Addr::new(192, 168, 1, 43),
                Ipv4Addr::new(192, 168, 1, 44),
                Ipv4Addr::new(192, 168, 1, 45),
                Ipv4Addr::new(192, 168, 1, 46),
            ]
        );
    }

    #[test]
    fn extracts_the_host_from_an_onvif_xaddr() {
        assert_eq!(
            host_from_xaddr("http://192.168.1.23/onvif/device_service").as_deref(),
            Some("192.168.1.23")
        );
    }

    #[test]
    fn extracts_the_port_from_an_onvif_xaddr() {
        assert_eq!(
            port_from_xaddr("http://192.168.1.23:8080/onvif/device_service"),
            Some(8080)
        );
        assert_eq!(
            port_from_xaddr("http://192.168.1.23/onvif/device_service"),
            Some(80)
        );
    }

    #[test]
    fn falls_back_to_onvif_scopes() {
        let scopes = vec![
            "onvif://www.onvif.org/name/Front%20Door".to_owned(),
            "onvif://www.onvif.org/hardware/RLC-811A".to_owned(),
        ];

        assert_eq!(
            scope_value(&scopes, "hardware").as_deref(),
            Some("RLC-811A")
        );
        assert_eq!(scope_value(&scopes, "name").as_deref(), Some("Front Door"));
    }

    #[test]
    fn avoids_repeating_identical_manufacturer_and_model() {
        let camera = NetworkCamera {
            address: "192.168.1.23".to_owned(),
            port: 80,
            manufacturer: Some("Reolink".to_owned()),
            model: "Reolink".to_owned(),
        };

        assert_eq!(camera.display_name(), "Reolink");
    }

    #[test]
    fn avoids_prefixing_manufacturer_when_model_already_starts_with_it() {
        let camera = NetworkCamera {
            address: "192.168.1.23".to_owned(),
            port: 80,
            manufacturer: Some("Canon".to_owned()),
            model: "Canon EOS R3".to_owned(),
        };

        assert_eq!(camera.display_name(), "Canon EOS R3");
    }

    #[test]
    fn normalizes_canon_inc_manufacturer() {
        assert_eq!(
            normalize_manufacturer("Canon Inc.").as_deref(),
            Some("Canon")
        );
    }

    #[test]
    fn recognizes_placeholder_models() {
        assert!(model_is_placeholder(UNKNOWN_CAMERA));
        assert!(model_is_placeholder(CANON_CCAPI_CAMERA));
        assert!(!model_is_placeholder("Canon EOS R3"));
    }
}
