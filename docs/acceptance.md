# Acceptance

A compiled build is not a passed installed-client test.

- Windows current-user installer / clean uninstall; real desktop protocol lookup.
- macOS installed application / Launch Services / login startup; clean uninstall.
- Agent unlocked, locked, absent, denied signature; no password fallback.
- Unknown / changed SSH host keys; fingerprint mismatch; malformed link fields.
- Simultaneous identical requests create one process; updated page path reuses it.
- Local service detection; conflicts leave unrelated processes untouched.
- An occupied preferred port selects a free loopback port; browser and connection list use the selected port.
- Reopening the original link, including a different preferred-port hint, reuses the same auto connection and updates the page path.
- An SSH bind race retries with another port, at most three times; authentication and host-key failures do not retry as port conflicts.
- An unrelated HTTP listener must not cause a successful browser handoff before SSH confirms forwarding.
- `fixedPort=true` preserves fixed callback origins and reports an occupied port without fallback.
- Open, renew, disconnect one, disconnect all, expiry removal on both platforms.
- Close-to-tray, login background start, cold and warm protocol activation.
- Source browser return in Edge, Firefox, Chrome and Safari where installed.
- Dark/light appearance, keyboard focus, reduced motion, native material inspection.
- Independent navigation/content scrolling; settings apply immediately and errors restore saved state.
- Structured requests proceed without an additional manager permission prompt.
- Startup protocol repair and explicitly disabled autostart preference.
- Signed/notarized distribution is checked separately from unsigned test artifacts.
