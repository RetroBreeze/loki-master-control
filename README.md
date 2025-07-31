# loki-master-control
Loki Control Center clone for Linux.

## Building the UI

The user interface lives in the `ui/` directory and is written in Rust with
GTK4. When building it you need the GTK4 and GLib development packages, e.g. on
Ubuntu:

```bash
sudo apt-get install libgtk-4-dev libglib2.0-dev
```

The crate also requires the [`gtk4-layer-shell`](https://github.com/wmww/gtk4-layer-shell)
library. This library is not currently available from Ubuntu repositories, so it
has to be built from source. Follow the instructions in the upstream repository
and install it (typically under `/usr/local`). Afterwards make sure the
`PKG_CONFIG_PATH` and `LD_LIBRARY_PATH` variables include the location where the
library was installed. For example:

```bash
export PKG_CONFIG_PATH=/usr/local/lib/pkgconfig:$PKG_CONFIG_PATH
export LD_LIBRARY_PATH=/usr/local/lib:$LD_LIBRARY_PATH
```

With the dependencies installed you can build the UI with Cargo:

```bash
cd ui
cargo build
```
