# xshim
XShim is a lightweight wrapper for Xorg, (hopefully) making X sessions as easy to start as wayland ones.
As display managers begin to drop native support for Xorg, XShim can be used as a polyfill.

XShim replaces: startx, xinit, xorg-rootless wrapper[^1], xauth, mcookie
[^1]: this is subject to change

XShim is not:
- A complete display manager.
- An implementation/extension of X11 protocol.
- A bridge between X11 and wayland (for that, see wayback, xwayland).

# caveats
One thing XShim can never polyfill is the logind session type, that is by design of systemd-logind. A high-level display manager sets this through PAM, and it is immutable afterwards. Note that this limitation is not exclusive to XShim. For example, startx sessions register as "tty".

# todo
- [ ] Examples of use
- [ ] Xinit compatibility mode
- [ ] Multi-seat support
- [ ] Better systemd integration
- [ ] Parsing of Xorg logs into journald format