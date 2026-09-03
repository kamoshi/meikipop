# MeikiPop - Universal Japanese OCR Popup Dictionary

Instantly look up Japanese words anywhere on your screen. MeikiPop uses optical character recognition (OCR) to read text from websites, games, scanned manga, and even hard-coded video subtitles, providing effortless dictionary lookups with the press of a key - or without one.

This project is a Rust fork and rewrite of the [original MeikiPop](https://github.com/rtr46/meikipop), with an emphasis on robust native support for macOS and Linux under Wayland.

https://github.com/user-attachments/assets/a1834197-3059-438c-a2dc-716e8ec9078f


## Platform support

| OS              | Supported |
| --------------- | :-------: |
| Linux (Wayland) | ✅        |
| Linux (X11)     | ❌         |
| macOS (14.0+)   | ✅        |
| Windows         | ❌         |

On Linux, we can use the XDG Desktop Portal to select a screen or window, then
receives frames and cursor metadata directly through PipeWire. A Wayland portal
implementation with cursor-metadata support is required.

On macOS, we can use ScreenCaptureKit for native source selection and screen
capture, and Core Graphics for pointer and window information. OCR can run
locally with MeikiOCR or, in theory, use Apple's Vision framework.


## Features

- **Works across applications:** Look up Japanese text in native Wayland and XWayland windows, as well as macOS applications. MeikiPop uses the system's screen-capture facilities, so no browser extensions, hooks, or application-specific integrations are required.
- **OCR-powered:** Reads Japanese text directly from images, making it useful for games, comics, videos, websites, and other applications.
- **Fast dictionary lookups:** The dictionary is preprocessed into an optimized format for responsive lookups.
- **Simple and intuitive:** Select a capture source, then point your cursor at Japanese text to display its dictionary entries.
- **Multiple OCR backends:** Includes the local MeikiOCR backend and Apple Vision on macOS.


## Installation

For now the easiest and recommended way to install, build and run this software
is to use Nix and the provided `flake.nix` which contains everything required to
make it work. You can also use `make` for some commands used to build it via
`nix`, consult an AI in case of confusion.


## License

Meikipop is licensed under the GNU General Public License v3.0. see the
`LICENSE` file for the full license text.
