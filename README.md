# DDRS - A dynamic DNS client written in Rust 🦀

## Features
* IP lookups via [STUN](https://en.wikipedia.org/wiki/STUN), HTTP(S), or local network interfaces
* Support for multiple DNS providers
* Support for multiple domains/subdomains
* Support for IPv4 and IPv6

## Config
The configuration file is in [TOML](https://toml.io/en/) format. The default location for the configuration file is `/etc/ddrs/config.toml`. A custom location can be specified with the `--config` flag.

### Options
* `versions` - IP version to fetch and update
* `dry_run` - Fetch the IP address but do not update the DNS records
* `stun_urls` - A list of STUN servers to use for IP lookups
* `http_ipv4` - A list of HTTP(S) URLs to use for IPv4 lookups
* `http_ipv6` - A list of HTTP(S) URLs to use for IPv6 lookups
* `source` - The source to use for IP lookups
  * `type` - The source type. Must be `stun`, `http`, or `interface`
  * `name` - Only required for `interface` source type, (e.g. `eth0`, `wlan0`)

### Default Config

```toml
versions = ["v4"]

# versions = ["v4", "v6"]

dry_run = false

stun_urls = [
  "stun://stun.l.google.com:19302",
  "stun://stun.cloudflare.com:3478",
  "stun://global.stun.twilio.com:3478",
]

http_ipv4 = [
  "https://api.ipify.org",
  "https://ipv4.icanhazip.com",
  "https://ipv4.seeip.org",
]

http_ipv6 = [
  "https://api6.ipify.org",
  "https://ipv6.icanhazip.com",
  "https://ipv6.seeip.org",
]

[source]
type = "stun"

# [source]
# type = "http"

# [source]
# type = "interface"
# name = "eth0"

[[providers]]
# Provider(s) configuration
```

## Providers

### Cloudflare
* `type` - The provider type. Must be `cloudflare`
* `zone` - The zone root domain to update
* `api_token` - Cloudflare API token with the `Zone.DNS Edit` permission
* `domains` - A list of domains to update
  * `name` - The full domain name to update (Required)
  * `ttl` - The TTL for the record, default is `1` (Auto)
  * `proxied` - Whether the record is proxied through Cloudflare, default is `false`
  * `comment` - A comment to add to the record

```toml
[[providers]]
type = "cloudflare"
zone = "domain.com"
api_token = "TOKEN"

[[providers.domains]]
name = "*.domain.com"
ttl = 1
proxied = false
comment = "Wildcard subdomain"

[[providers.domains]]
name = "domain.com"
ttl = 1
proxied = false
comment = "Root domain"
```

## Deployment
* Logging can be configured with the [RUST_LOG](https://docs.rs/env_logger/latest/env_logger/#enabling-logging) environment variable. By default, the log level is set to `info`. For more verbose logging, set the environment variable to `RUST_LOG=ddrs=debug`.

### Docker Compose
* Create configuration file `config.toml`
* Create a `docker-compose.yml` file
* Run `docker-compose up -d`

```yaml
services:
  ddrs:
    image: ghcr.io/jakewmeyer/ddrs:latest
    container_name: ddrs
    restart: unless-stopped
    network_mode: "host"
    volumes:
      - ./config.toml:/etc/ddrs/config.toml
```

### Systemd
* Save the binary to `/usr/local/bin/ddrs`
* Save the configuration file to `/etc/ddrs/config.toml`
* Create a systemd service file at `/etc/systemd/system/ddrs.service`
* Reload systemd with `sudo systemctl daemon-reload`
* Start the service with `sudo systemctl start ddrs`
* Enable the service with `sudo systemctl enable ddrs`

```ini
# Systemd service file

[Unit]
Description=Dynamic DNS Rust Service (DDRS)
After=network.target network-online.target
Requires=network-online.target

[Service]
Type=simple
User=ddrs
ExecStart=/usr/local/bin/ddrs
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal
ProtectSystem=full

# Hardening
PrivateDevices=true
PrivateTmp=true
NoNewPrivileges=true
ProtectHome=true
ProtectControlGroups=true
ProtectKernelModules=true
ProtectKernelTunables=true
RestrictSUIDSGID=true

[Install]
WantedBy=multi-user.target
```
