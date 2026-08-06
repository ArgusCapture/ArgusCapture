<!--
Copyright (C) 2026 The Argus Capture community

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program. If not, see <https://www.gnu.org/licenses/>.
-->

# Contributing guide

**Want to contribute? Great!**

All contributions are more than welcome ! This includes bug reports, bug fixes, enhancements, features, questions, ideas,
and documentation.

This document will hopefully help you contribute to Argus Capture.

* [Legal](#legal)
* [Reporting an issue](#reporting-an-issue)
* [Setup your build environment](#setup-your-build-environment)
* [Building the main branch](#building-the-main-branch)
* [Before you contribute](#before-you-contribute)
* [Code reviews](#code-reviews)
* [Coding Guidelines](#coding-guidelines)
* [Discuss a Feature](#discuss-a-feature)
* [Development](#development)
* [Code Style](#code-style)

## Legal

All contributions to Argus Capture are licensed under the [GNU General Public License v3.0](https://www.gnu.org/licenses/gpl-3.0.en.html).

## Reporting an issue

This project uses GitHub issues to manage the issues. Open an issue directly in GitHub.

If you believe you found a bug, and it's likely possible, please indicate a way to reproduce it, what you are seeing and what you would expect to see.
Don't forget to indicate your Argus Capture version.

## Setup your build environment

For Red Hat RPM based distributions use the following command:

```bash
dnf install git rust rust-std-static cargo rustfmt clippy
```
## Setup your build environment in Windows
1. You can install Rust using [rustup](https://rust-lang.org/tools/install/)
2. Install MSYS2 and add it to your PATH. You can download it from [MSYS2](https://www.msys2.org/).
3. Open the MSYS2 Blue terminal (not yellow or purple) and install the necessary dependencies.

```bash
pacman -S mingw-w64-x86_64-pkg-config mingw-w64-x86_64-libgphoto2 mingw-w64-x86_64-clang mingw-w64-x86_64-gtk4 mingw-w64-x86_64-gstreamer mingw-w64-x86_64-gst-plugins-base mingw-w64-x86_64-gst-plugins-good mingw-w64-x86_64-gst-plugins-bad mingw-w64-x86_64-gcc-libs

```

4. Add the following environment variables to your system using the PowerShell.

```shell
$env:LIBCLANG_PATH = "C:\msys64\mingw64\bin"
$env:PKG_CONFIG_PATH = "C:\msys64\mingw64\lib\pkgconfig"
$env:PATH += ";C:\msys64\mingw64\bin"
```

5. Open a new terminal and run `cargo build` in the project directory to build the project.


In order to get the necessary dependencies.

## Building the main branch

To build the `main` branch:

```
git clone https://github.com/ArgusCapture/ArgusCapture.git
cd ArgusCapture
make build
cd target/debug
./argus-capture
```

and you will have a running instance.

## Before you contribute

To contribute, use GitHub Pull Requests, from your **own** fork.

Also, make sure you have set up your Git authorship correctly:

```
git config --global user.name "Your Full Name"
git config --global user.email your.email@example.com
```

We use this information to acknowledge your contributions in release announcements.

## Code reviews

GitHub pull requests can be reviewed by all such that input can be given to the author(s).

See [GitHub Pull Request Review Process](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/reviewing-changes-in-pull-requests/about-pull-request-reviews)
for more information.

## Coding Guidelines

* Discuss the feature
* Do development
  + Follow the code style
* Commits should be atomic and semantic. Therefore, squash your pull request before submission and keep it rebased until merged
  + If your feature has independent parts submit those as separate pull requests

## Discuss a Feature

You can discuss bug reports, enhancements and features in our [forum](https://github.com/ArgusCapture/ArgusCapture/discussions).

Once there is an agreement on the development plan you can open an issue that will used for reference in the pull request.

## Development

You can follow this workflow for your development.

Add your repository

```
git clone git@github.com:yourname/ArgusCapture.git
cd ArgusCapture
git remote add upstream https://github.com/ArgusCapture/ArgusCapture.git
```

Create a work branch

```
git checkout -b mywork main
```

During development

```
git commit -a -m "[#issue] My feature"
git push -f origin mywork
```

If you have more commits then squash them

```
git rebase -i HEAD~2
git push -f origin mywork
```

If the `main` branch changes then

```
git fetch upstream
git rebase -i upstream/main
git push -f origin mywork
```

as all pull requests should be squashed and rebased.

In your first pull request you need to add yourself to the `AUTHORS` file.

## Code Style

Please, follow the coding style of the project.

You can use the [rustfmt](https://github.com/rust-lang/rustfmt) tool to help with the formatting, by running

```
cargo fmt
```

and verify the changes.
