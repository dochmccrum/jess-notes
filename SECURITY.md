# Security

Please report vulnerabilities privately, through GitHub: *Security → Report a vulnerability* on
this repository. Don't use public issues for them. You should get a reply within a week.

Supported: the latest release.

How it’s built (DESIGN §14 has the detail): one account per server, with the password hashed by
argon2id and sign-in rate-limited per IP. Each device gets its own revocable token, and tokens
are stored hashed. The server expects HTTPS from a reverse proxy, and the web app ships with a
strict Content-Security-Policy. Attachments are only served to signed-in devices, and SVGs are
sandboxed. The image runs as a non-root user.

Not covered: the vault isn't encrypted at rest, on the server or on devices. Use disk or
device encryption for that.
