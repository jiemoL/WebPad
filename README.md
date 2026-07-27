# WebPad

A WebSocket-based virtual gamepad simulator built with Rust. Turn your phone/tablet into a virtual Xbox 360 controller.

## Features

- 🎮 **Virtual Xbox 360 Controller** - Simulates Xbox 360 controller via ViGEmBus driver (Windows only)
- 📱 **Mobile-Friendly Web Interface** - Touchscreen gamepad with joysticks, triggers, and buttons
- 🔐 **Secure Authentication** - Argon2id password hashing with exponential backoff against brute force
- 🔒 **TLS by Default** - HTTPS/WSS encryption, HTTP auto-redirects to HTTPS
- ❤️ **Heartbeat Detection** - Automatic disconnection for inactive clients
- 🌐 **UPnP Port Mapping** - Automatic port forwarding for external access
- 📊 **Connection Limiting** - Protection against DoS attacks

## Tech Stack

| Component | Technology |
|-----------|------------|
| Language | Rust (Edition 2021) |
| Web Framework | Axum 0.8 |
| Web Server | axum-server 0.7 (TLS) |
| Virtual Gamepad | ViGEmClient 0.1.4 |
| TLS | rustls + rcgen |
| Password Hashing | Argon2id |
| Serialization | serde + serde_json |

## Prerequisites

- **Windows 10/11** (full gamepad emulation) or any OS for Web server only
- **Rust** stable toolchain
- **ViGEmBus Driver** - Required for gamepad emulation on Windows
  - Download: https://github.com/ViGEm/ViGEmBus/releases

## Installation

```bash
# Clone the repository
git clone https://github.com/jiemoL/WebPad.git
cd WebPad

# Build
cargo build --release

# Run
cargo run --release
```

## Usage

1. Run the server:
   ```bash
   cargo run --release
   ```

2. Note the password shown in the console output.

3. Open your mobile browser and navigate to:
   ```
   https://<your-computer-ip>:8443/
   ```

4. Enter the password when prompted.

5. Start using the virtual gamepad!

## Configuration

The configuration file `webpad.toml` is created in the same directory as the executable:

```toml
port = 8443
http_redirect_port = 8080
password = "your_password_here"
# cert_path = "path/to/cert.pem"
# key_path = "path/to/key.pem"
enable_upnp = true
heartbeat_timeout_secs = 30
max_connections = 8
max_unauth_connections = 3
```

### Command Line Options

```
WebPad 0.1.0
Virtual gamepad simulator

USAGE:
    webpad.exe [OPTIONS]

OPTIONS:
    -p, --port <PORT>                    Listening port
    -w, --password <PASSWORD>            Connection password
        --no-upnp                        Disable UPnP port mapping
        --heartbeat-timeout <SECONDS>    Heartbeat timeout in seconds
    -h, --help                           Print help information
    -V, --version                        Print version information
```

## Web Interface

The web interface provides:
- Dual analog joysticks
- ABXY action buttons
- D-pad (directional pad)
- LB/RB shoulder buttons
- Back/Start buttons
- Left/Right triggers
- Password authentication dialog
- Connection status indicator
- Automatic reconnection with exponential backoff

## Security

- TLS encryption enabled by default
- Argon2id password hashing (no SHA-256 fallback for new hashes)
- Connection limiting (max 8 total, 3 unauthenticated)
- Authentication failure exponential backoff (500ms - 10s)
- Maximum 5 authentication failures per connection
- Session tokens (32-byte random) for reconnection
- Fail-closed: No password = all authentication fails

## Project Structure

```
src/
├── main.rs              # Entry point
├── config.rs            # Configuration management
├── protocol.rs          # WebSocket message protocol
├── auth.rs              # Authentication manager
├── password.rs          # Password hashing utilities
├── gamepad/
│   ├── mod.rs           # Module exports
│   ├── types.rs         # Gamepad state types
│   └── manager.rs       # ViGEmBus controller management
├── upnp/
│   ├── mod.rs           # Module exports
│   └── mapper.rs        # UPnP port mapping
└── web/
    ├── mod.rs           # Module exports
    ├── server.rs        # Axum router and state
    └── handler.rs       # HTTP/WebSocket handlers
```

## Contributing

Contributions are welcome! Please feel free to submit issues and pull requests.

## License

MIT License