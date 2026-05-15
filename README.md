# xshim
xshim is a lightweight wrapper for Xorg, (hopefully) making X sessions as easy to start as wayland ones.
As display managers begin to drop native support for Xorg, xshim can be used as a polyfill.

xshim replaces: startx, xinit, xorg-rootless wrapper[^1], xauth, mcookie
[^1]: this is subject to change

xshim is also available as a library. Any rust-based session manager can use it to support X sessions. Just call `libxshim::setup_xorg`!

xshim is not:
- A complete display manager.
- An implementation/extension of X11 protocol.
- A bridge between X11 and wayland (for that, see wayback, xwayland).


# todo
- [ ] Examples of use
- [x] Xinit compatibility mode
- [x] Systemd integration
- [ ] Parsing of Xorg logs into journald format
- [x] Library mode
- [x] Async (CLI)
- [ ] XResources support

# features

library:
- bon: generates a builder for the configuration
- client: makes your app an Xorg client after setup
- xrdb (WIP): loads Xresources

cli:
- dbus: required feature for full systemd intergration (env, transient units)
- xrdb (WIP): loads Xresources
