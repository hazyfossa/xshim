# xshim
XShim is a lightweight wrapper for Xorg, (hopefully) making X sessions as easy to start as wayland ones.
As display managers begin to drop native support for Xorg, XShim can be used as a polyfill.

xshim-cli replaces: startx, xinit, xorg-rootless wrapper[^1], xauth, mcookie
[^1]: this is subject to change

xshim (library) allows any rust-based session manager to support Xorg sessions with a simple call to xshim::setup_xorg

XShim is not:
- A complete display manager.
- An implementation/extension of X11 protocol.
- A bridge between X11 and wayland (for that, see wayback, xwayland).


# todo
- [ ] Examples of use
- [x] Xinit compatibility mode
- [x] Better systemd integration
- [ ] Parsing of Xorg logs into journald format
- [x] Library mode
- [ ] Async via tokio