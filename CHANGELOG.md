# Unreleased

Nothing Yet!

# Version 0.5.20 (2024-10-05)

* Add `smtp_send(url, from, to, subject, body)` function to send email
* Add MQTT support: `publish("mqtt://servername:1883/topic", body)`
* Add `system2(cmd)`: different from `system(cmd)`, and it will return an array with `code`, `stdout`, `stderr`.
* Add `cargo-binstall` and `cargo-dist` support
