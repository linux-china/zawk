# Unreleased

Nothing Yet!

# Version 0.5.25 (2024-12-29)

* Add `getenv("NAME", "default value")` function to get environment variable
* Fix `substr(s,index,len)`: 0 and negative index support now and same with gawk.

# Version 0.5.24 (2024-12-22)

* Fix `_join` with wrong sequence

# Version 0.5.23 (2024-12-10)

* Add `chars($1)` function to return char array of text
* Use fs if sep empty in `split(text,arr,sep)`

# Version 0.5.22 (2024-10-18)

* Add `eval(formula, context)` function for math calculation
* Add [Resend](https://resend.com/emails) mail service support for `send_mail()`, and environment variable is `RESEND_API_KEY`.
* Documentation to make associative array quickly: `array[$1] = $2`, `arr = record("{host:localhost,port:1234}")`, `arr = pairs("a=b,c=d")`.

# Version 0.5.20 (2024-10-05)

* Add `smtp_send(url, from, to, subject, body)` function to send email
* Add MQTT support: `publish("mqtt://servername:1883/topic", body)`
* Add `system2(cmd)`: different from `system(cmd)`, and it will return an array with `code`, `stdout`, `stderr`.
* Add `cargo-binstall` and `cargo-dist` support
