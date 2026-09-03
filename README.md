# MeikiPop - Universal Japanese OCR Popup Dictionary

Instantly look up Japanese words anywhere on your screen. MeikiPop uses optical character recognition (OCR) to read text from websites, games, scanned manga, and even hard-coded video subtitles, providing effortless dictionary lookups with the press of a key - or without one.

This project is a Rust fork and rewrite of the [original MeikiPop](https://github.com/rtr46/meikipop), with an emphasis on robust native support for macOS and Linux under Wayland.

https://github.com/user-attachments/assets/a1834197-3059-438c-a2dc-716e8ec9078f



## Features

- **Works across applications:** Look up Japanese text in native Wayland and XWayland windows, as well as macOS applications. MeikiPop uses the system's screen-capture facilities, so no browser extensions, hooks, or application-specific integrations are required.
- **OCR-powered:** Reads Japanese text directly from images, making it useful for games, comics, videos, websites, and other applications.
- **Fast dictionary lookups:** The dictionary is preprocessed into an optimized format for responsive lookups.
- **Simple and intuitive:** Select a capture source, then point your cursor at Japanese text to display its dictionary entries.
- **Multiple OCR backends:** Includes the local MeikiOCR backend and Apple Vision on macOS.


## philosophy & limitations

meikipop is designed to do one thing and do it exceptionally well: provide fast, frictionless, on-screen dictionary lookups.

it is heavily inspired by the philosophy of [Nazeka](https://github.com/wareya/nazeka), a fantastic browser-based popup dictionary, and aims to bring that seamless experience to the entire desktop. it also draws inspiration from the ocr architecture of [owocr](https://github.com/AuroraWright/owocr/tree/master/owocr).

to maintain this focus, there are a few things meikipop is **not**:

*   **it is not an srs-mining tool.** meikipop does not include functionality to automatically create flashcards for programs like anki.
*   **it is not a multi-dictionary tool.** while meikipops lets you import yomitan dictionaries, it is designed to run best with a single, semi-custom jmdict+kanjidic dictionary. 

## installation

there are a few different ways to install and run meikipop. note that when meikipop is started for the first time, a dictionary and ocr models may be downloaded.

### easiest: prepackaged binaries

just download, unpack and start the executable binary. no python installation required:
* https://github.com/rtr46/meikipop/releases/latest

### recommended: install via pypi

if you already have python 3.10+ installed, this is the most flexible option that lets you run directly from source and enables you to edit the program.

```bash
#... activate your environment if any
pip install --upgrade meikipop
meikipop  # run the application
```

### for development: editable install

if you are planning to modify, fork or contribute to meikipop, it is best to checkout this repo and create an editable install

```bash
#... activate your environment if any
git clone https://github.com/rtr46/meikipop.git
cd meikipop
pip install -e .
meikipop  # run the application
```

### platform support

* **windows, linux (x11)** - these are the primary supported platforms
* **macos** - supported thanks to community contributions
* **linux (wayland)** - it can work in principle thanks to community contributions, but may require additional trouble shooting

see for platform specific setup details:
<details>
<summary>macos</summary>

* go to **System Preferences** > **Security & Privacy** > **Privacy**
* add/enable your terminal app in **Input Monitoring**, **Screen Recording** and **Accessibility**

note that there may be problems when using python 3.14. use one of [these workarounds](https://github.com/rtr46/meikipop/issues/43) if necessary.
</details>

<details>
<summary>wayland (alpha)</summary>

it is possible to run meikipop on wayland in principle, but depending on your specific setup you may need to take additional steps like installing additional dependencies, fixing some of the wayland specific code or changing some of your setup. since the wayland eco system is terribly fragmented and deliberately prevents apps like meikipop from working natively, don't expect any support, but feel free to open an issue regardless.

The native Linux backend uses the XDG ScreenCast portal and PipeWire directly.
It requires a portal implementation that supports cursor metadata, such as the
current KDE/KWin and GNOME/Mutter implementations. XWayland is not required for
screen capture or cursor tracking.

Install the PipeWire development package when building from source (typically
`pipewire-devel` on Fedora or `libpipewire-0.3-dev` on Ubuntu).
</details>

## how to use

1.  run the application (`meikipop`).
2.  the first time you run the app in `region` mode, you will be prompted to select an area of your screen to scan.
3.  move your mouse over any japanese text on your screen.
4.  a popup with dictionary entries will appear.
5.  **right-click the system tray icon** to open the settings, reselect the scan region or quit the application.

## configuration

you can fully customize meikipop's behavior and appearance. right-click the tray icon and choose "settings" to open the configuration gui.

changes are saved to a platform-specific user data directory which contains `config.ini` and `dictionary.pkl`:
- windows: `%LOCALAPPDATA%\meikipop\`
- linux: `~/.config/meikipop/`
- macos: `~/Library/Application Support/meikipop/`

## ocr backend

meikipop currently uses the native meikiocr backend. it is designed for
japanese video-game text and runs locally on the cpu.

## building your own dictionary (optional)

in case you want to update your dictionary you can simply run:

```bash
meikipop build-dict
```

if you want to import a yomitan dictionary that is possible as well. you can import multiple yomitan dictionaries at once, but be aware that this will overwrite your default dictionary:

```bash
# try to keep as much of the dictionary's original formatting
meikipop import-yomitan-dict-html my_yomitan_dict.zip
# or create a compact, text only dictionary
meikipop import-yomitan-dict-text my_yomitan_dict.zip
# or import multiple dictionaries at once
meikipop import-yomitan-dict-text dict1.zip dict2.zip
```

## license

meikipop is licensed under the GNU General Public License v3.0. see the `LICENSE` file for the full license text.
