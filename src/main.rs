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

use gphoto2::{Context, Result, library_version};

fn main() -> Result<()> {
    println!("argus-capture");
    println!(
        "Using libgphoto2 {}",
        library_version().unwrap_or("unknown")
    );

    let context = Context::new()?;
    let cameras: Vec<_> = context.list_cameras().wait()?.collect();

    if cameras.is_empty() {
        println!("No cameras detected. Connect a supported camera and run again.");
        return Ok(());
    }

    println!("Detected cameras:");
    for camera in &cameras {
        println!("- {} on {}", camera.model, camera.port);
    }

    let camera = context.get_camera(&cameras[0]).wait()?;
    println!("Selected camera: {}", camera.abilities().model());

    match camera.summary() {
        Ok(summary) => println!("\nSummary:\n{summary}"),
        Err(error) => eprintln!("Could not read camera summary: {error}"),
    }

    Ok(())
}
