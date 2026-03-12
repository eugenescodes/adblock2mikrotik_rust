# adblock2mikrotik_rust

Convert ad-blocking filter lists to MikroTik RouterOS DNS adlist format.

> [!NOTE]
> This is a Rust rewrite of [adblock2mikrotik](https://github.com/eugenescodes/adblock2mikrotik)

> [!TIP]
> Ready-to-use URL for RouterOS:
> `https://raw.githubusercontent.com/eugenescodes/adblock2mikrotik_rust/refs/heads/main/hosts.txt`

## Overview

Transforms popular ad-blocking filter lists (Hagezi) into a compact format compatible with the MikroTik RouterOS 7.15+ DNS adlist feature.
Optimized for memory-constrained low-resource devices like the [RB951Ui-2nD hAP](https://mikrotik.com/product/RB951Ui-2nD) (which has 16 MB storage).

### Sources

| List | Description |
| --- | --- |
| [Hagezi Multi PRO mini](https://github.com/hagezi/dns-blocklists?tab=readme-ov-file#ledger-multi-pro-mini-recommended-for-browsermobile-adblockers-) | General ad/tracker blocking |
| [Hagezi TIF mini](https://github.com/hagezi/dns-blocklists?tab=readme-ov-file#closed_lock_with_key-threat-intelligence-feeds---mini-version-) | Threat intelligence feeds |
| [Hagezi Gambling mini](https://github.com/hagezi/dns-blocklists?tab=readme-ov-file#slot_machine-gambling---mini-version-) | Gambling sites |

## Features

- Converts `||example.com^` rules to MikroTik DNS adlist format (`0.0.0.0 example.com`)
- Deduplicates entries across all sources
- Validates domains against RFC label rules (rejects double-dots, leading/trailing hyphens)
- Pre-filters comments and empty lines for efficiency
- Compatible with RouterOS 7.15+

## Usage

### Option 1 — Cargo (recommended)

```bash
# Install Rust if not already installed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and run
git clone https://github.com/eugenescodes/adblock2mikrotik_rust
cd adblock2mikrotik_rust
cargo run --release
```

After running, `hosts.txt` is created in the current directory.

### Option 2 — Docker

```bash
docker build -t adblock2mikrotik_rust .
```

> [!IMPORTANT]
> If `hosts.txt` does not exist in your current directory, Docker might create it as a directory. Create the file first:
>
> ```bash
> touch hosts.txt
> ```

```bash
# Linux / macOS
docker run --rm --user $(id -u):$(id -g) -v "$(pwd)":/output adblock2mikrotik_rust

# Windows (PowerShell)
docker run --rm -v "${PWD}:/output" adblock2mikrotik_rust
```

> [!NOTE]
> The `-v` flag mounts your current directory into the container at `/output`.
> The binary writes `hosts.txt` to `/output`, so the file appears directly
> in your current directory on the host — no manual copying needed.
>
> On Linux, `--user $(id -u):$(id -g)` ensures the output file is owned by
> your current user. Not required on macOS or Windows (Docker Desktop handles this automatically).

## MikroTik RouterOS Integration

### Add adlist via URL

```routeros
/ip/dns/adlist add url=https://raw.githubusercontent.com/eugenescodes/adblock2mikrotik_rust/refs/heads/main/hosts.txt ssl-verify=no
```

### Optional: enable SSL verification

If you want to use `ssl-verify=yes`, you can download and import [CA certificates](https://curl.se/docs/caextract.html) using the following commands:

```routeros
/tool fetch url=https://curl.se/ca/cacert.pem
/certificate import file-name=cacert.pem passphrase=""
/ip/dns/adlist add url=https://raw.githubusercontent.com/eugenescodes/adblock2mikrotik_rust/refs/heads/main/hosts.txt ssl-verify=yes
```

### Add adlist from local file

```routeros
/ip/dns/adlist add file=hosts.txt
```

See also the official MikroTik documentation:

- [DNS Adlist - MikroTik Documentation](https://help.mikrotik.com/docs/spaces/ROS/pages/37748767/DNS#DNS-Adlist)
- [Certificates - MikroTik Documentation](https://help.mikrotik.com/docs/spaces/ROS/pages/2555969/Certificates)

## Configuration

By default, the script uses three pre-configured Hagezi filter lists. You can customize which sources are used by creating a `config.toml` file:

### Customize sources

1. Copy the example configuration:

```bash
cp config.toml.example config.toml
```

1. Edit `config.toml` to add or remove sources:

```toml
[sources]
urls = [
    "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/pro.mini.txt",
    "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/tif.mini.txt",
    "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/gambling.mini.txt",
]
```

1. Run the converter:

```bash
cargo run --release
```

The script will automatically load sources from `config.toml`. If the file doesn't exist, it falls back to the default sources above.

### Finding additional filter lists

You can use any blocklist in AdBlock format (`||domain.com^` syntax)

For more Hagezi lists, visit the [Hagezi DNS blocklists repository](https://github.com/hagezi/dns-blocklists)

## Development

This project uses [Cargo](https://doc.rust-lang.org/cargo/) for dependency management and [Clippy](https://github.com/rust-lang/rust-clippy) + [rustfmt](https://github.com/rust-lang/rustfmt) for linting/formatting.

### Prerequisites

Install Rust via [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Lint and format

```bash
# format
cargo fmt --all  

# lint
cargo clippy --all-targets --all-features -- -D warnings  
```

### Tests

```bash
cargo test --verbose
cargo test --doc
```

## Contributing

1. Open a [GitHub issue](https://github.com/eugenescodes/adblock2mikrotik_rust/issues) to discuss major changes before starting work.
2. Fork the repo and create a feature branch: `git checkout -b feature/your-feature`
3. Make your changes and run tests: `cargo test --verbose`
4. Commit with a clear message and push to your fork.
5. Open a Pull Request targeting `main` with a description of what and why.

## License

[GNU GPL v3.0](LICENSE)

## Acknowledgments

- [Hagezi](https://github.com/hagezi/dns-blocklists) for maintaining comprehensive filter lists
- MikroTik for the DNS adlist feature in RouterOS 7.15+

---

> This tool is not affiliated with MikroTik.
