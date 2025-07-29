> [!NOTE]
> This repository provides a Rust version of the - https://github.com/eugenescodes/adblock2mikrotik

> [!TIP]
> URL to add in RouterOS: https://raw.githubusercontent.com/eugenescodes/adblock2mikrotik_rust/refs/heads/main/hosts.txt

# adblock2mikrotik

Convert AdBlock/AdGuard filter lists to MikroTik RouterOS DNS adlist format.

## Overview

A conversion utility designed to transform popular ad-blocking filter lists (AdGuard and Hagezi) into a compact, memory-efficient format compatible with MikroTik RouterOS 7.15+ DNS adlist feature.

### Source Filter Lists

- Hagezi [Multi PRO mini](https://github.com/hagezi/dns-blocklists?tab=readme-ov-file#ledger-multi-pro-mini-recommended-for-browsermobile-adblockers-): [link to file on adblock format](https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/pro.mini.txt)
- Hagezi [Threat Intelligence Feeds - Mini version](https://github.com/hagezi/dns-blocklists?tab=readme-ov-file#closed_lock_with_key-threat-intelligence-feeds---mini-version-): [link to file on adblock format](https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/tif.mini.txt)

The primary goal is to create a minimal, optimized host file that addresses the limited memory constraints of low-resource devices like the ```hAP series``` (which has 16 MB storage but less than 3 MB free after upgrading to RouterOS 7), for example the [RB951Ui-2nD hAP](https://mikrotik.com/product/RB951Ui-2nD) router

## Features

- Converts AdBlock/AdGuard syntax to MikroTik DNS adlist format
- Removes duplicates and optimizes storage space
- Supports multiple input filter list formats
- Compatible with RouterOS 7.15 and newer
- Preserves only domain-based rules
- Removes comments and unnecessary elements

Supports common AdBlock/AdGuard filter rules, including:

- Domain rules (`||example.com^`)
- Basic URL rules
- Comment lines (automatically removed)

Generates a clean list of domains in MikroTik DNS adlist format:

```text
0.0.0.0 example.com
0.0.0.0 ads.example.net
0.0.0.0 tracking.example.org
```

## Use on MikroTik

How to implement DNS adblocking on MikroTik RouterOS 7.15+ using online blocklists. You must have active internet connextion and basic RouterOS configuration knowledge.
To add a URL-based adlist for DNS adblocking, use the following command in the router terminal:

```routeros
/ip/dns/adlist add url=https://raw.githubusercontent.com/eugenescodes/adblock2mikrotik_rust/refs/heads/main/hosts.txt ssl-verify=no
```

If you want to use properties -`ssl-verify=yes` you can download and import [CA certificates](https://curl.se/docs/caextract.html) use next commands:

```routeros
/tool fetch url=https://curl.se/ca/cacert.pem
```

The resulting output should be:

```routeros
      status: finished
  downloaded: 225KiB  
       total: 225KiB  
    duration: 1s 
```

Then run next command:

```routeros
/certificate import file-name=cacert.pem passphrase=""                                                  
```

Output should be:

```routeros
certificates-imported: 149
     private-keys-imported:   0
            files-imported:   0
       decryption-failures:   0
  keys-with-no-certificate:   0
```

After that run next command:

```routeros
/ip/dns/adlist add url=_https://raw.githubusercontent.com/eugenescodes/adblock2mikrotik_rust/refs/heads/main/hosts.txt ssl-verify=yes
```

For a comprehensive guide on DNS adblocking and adlist configuration, refer to the official MikroTik documentation:

- [DNS Adlist - MikroTik Documentation](https://help.mikrotik.com/docs/spaces/ROS/pages/37748767/DNS#DNS-Adlist)
- [Certificates - MikroTik Documentation](https://help.mikrotik.com/docs/spaces/ROS/pages/2555969/Certificates)

## Use on local

To use the tool locally, follow these steps:

1. Clone the repository and navigate into the project directory:

```bash
git clone https://github.com/eugenescodes/adblock2mikrotik_rust.git
cd adblock2mikrotik_rust
```

2. Build the project in release mode:

```bash
cargo build --release
```

3. Run the application:

```bash
cargo run --release
```

This will generate or update the `hosts.txt` file in the project directory.

4. To apply the generated hosts file on your MikroTik RouterOS, upload the `hosts.txt` file to the router and add it as a DNS adlist:

```routeros
/ip/dns/adlist add file=hosts.txt
```

For more detailed information on DNS adblocking and adlist configuration, refer to the official MikroTik documentation:

- [DNS Adlist - MikroTik Documentation](https://help.mikrotik.com/docs/spaces/ROS/pages/37748767/DNS#DNS-Adlist)

## License

This project is licensed under the GNU General Public License v3.0 - see the [LICENSE](LICENSE) file for details.

## Acknowledgments

- Hagezi communities for maintaining comprehensive filter lists
- MikroTik for implementing DNS adlist feature in RouterOS 7.15

## Note

This tool is not affiliated with MikroTik
