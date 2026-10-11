"""Stages an unsigned Windows build of Buzz and zips it.

Lays the app out as GStreamer's own installation is laid out, so the
bundled GStreamer finds its plugins and GLib its GIO modules relative to
their DLLs, with no GStreamer installed:

    Buzz/bin/buzz-gpui.exe, buzzd.exe and every DLL they and the plugins load
    Buzz/lib/gstreamer-1.0/<plugins>
    Buzz/lib/gio/modules/<modules>

Usage: python scripts/package-windows.py <gstreamer root>
(the root holds bin/ and lib/, e.g. C:/gstreamer/1.0/msvc_x86_64).
Needs `pip install pefile`. Writes dist/Buzz-<version>-windows-x86_64.zip.
"""

import pathlib
import shutil
import sys
import tomllib

import pefile

# What playback, sounds and voice notes use: playbin and its decoders,
# app sources and sinks, audio output and capture, and the MP4 encoder path.
PLUGINS = [
    "app", "audioconvert", "audioparsers", "audioresample", "autodetect",
    "coreelements", "directsound", "dav1d", "isomp4", "libav", "matroska",
    "mpg123", "ogg", "opus", "playback", "soup", "typefindfunctions",
    "videoconvertscale", "videoparsersbad", "volume", "vorbis", "vpx",
    "wasapi", "wasapi2", "wavparse", "flac", "audiotestsrc", "rawparse",
    "id3demux", "pbtypes", "gio", "tcp", "adaptivedemux2", "dash", "hls",
]


def imports(path):
    pe = pefile.PE(str(path), fast_load=True)
    pe.parse_data_directories(
        directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_IMPORT"],
                     pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_DELAY_IMPORT"]])
    names = []
    for attr in ("DIRECTORY_ENTRY_IMPORT", "DIRECTORY_ENTRY_DELAY_IMPORT"):
        for entry in getattr(pe, attr, []):
            names.append(entry.dll.decode().lower())
    pe.close()
    return names


def main():
    root = pathlib.Path(sys.argv[1])
    repo = pathlib.Path(__file__).resolve().parent.parent
    with open(repo / "Cargo.toml", "rb") as manifest:
        version = tomllib.load(manifest)["workspace"]["package"]["version"]
    stage = repo / "dist" / "Buzz"
    shutil.rmtree(stage, ignore_errors=True)
    (stage / "bin").mkdir(parents=True)
    (stage / "lib" / "gstreamer-1.0").mkdir(parents=True)
    (stage / "lib" / "gio" / "modules").mkdir(parents=True)

    available = {p.name.lower(): p for p in (root / "bin").glob("*.dll")}
    # The Visual C++ runtime, deployed app-locally as Microsoft allows, for
    # machines without the redistributable.
    system = pathlib.Path("C:/Windows/System32")
    for name in ("vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"):
        if (system / name).exists():
            available[name] = system / name
    pending = []
    for exe in ("buzz-gpui.exe", "buzzd.exe"):
        shutil.copy2(repo / "target" / "release" / exe, stage / "bin")
        pending.append(stage / "bin" / exe)
    for name in PLUGINS:
        plugin = root / "lib" / "gstreamer-1.0" / f"gst{name}.dll"
        if plugin.exists():
            shutil.copy2(plugin, stage / "lib" / "gstreamer-1.0")
            pending.append(plugin)
        else:
            print(f"plugin {name} is not in this GStreamer; skipped")
    # The soup plugin loads libsoup at run time, not through its imports.
    seeds = [name for name in available if "soup" in name]
    for module in (root / "lib" / "gio" / "modules").glob("*.dll"):
        shutil.copy2(module, stage / "lib" / "gio" / "modules")
        pending.append(module)

    # Every DLL reachable from the binaries and plugins that GStreamer ships;
    # the rest are Windows' own.
    copied = set()

    def take(name):
        copied.add(name)
        shutil.copy2(available[name], stage / "bin")
        pending.append(available[name])

    for name in seeds:
        take(name)
    while pending:
        for name in imports(pending.pop()):
            if name in available and name not in copied:
                take(name)
    print(f"bundled {len(copied)} DLLs")

    for extra in ("LICENSE", "NOTICE.md"):
        shutil.copy2(repo / extra, stage)
    archive = repo / "dist" / f"Buzz-{version}-windows-x86_64"
    shutil.make_archive(str(archive), "zip", stage.parent, "Buzz")
    print(f"{archive}.zip")


if __name__ == "__main__":
    main()
