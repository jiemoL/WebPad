# WebPad Code Wiki

## 目录

1. [项目概述](#项目概述)
2. [整体架构](#整体架构)
3. [模块详解](#模块详解)
4. [关键数据结构与协议](#关键数据结构与协议)
5. [核心流程](#核心流程)
6. [配置说明](#配置说明)
7. [安全机制](#安全机制)
8. [依赖关系](#依赖关系)
9. [构建与运行](#构建与运行)
10. [测试体系](#测试体系)

---

## 项目概述

**WebPad** 是一个基于 Web 的虚拟游戏手柄模拟器。它允许用户通过手机/平板等移动设备的浏览器，将触屏操作转化为虚拟 Xbox 360 手柄输入，在 Windows 电脑上模拟真实手柄的功能。

### 核心特性

- **虚拟 Xbox 360 手柄模拟**（基于 ViGEmBus 驱动，仅 Windows）
- **Web 端触屏手柄界面**，支持摇杆、扳机、ABXY、十字键、肩键等
- **WebSocket 实时通信**，低延迟传输手柄状态
- **TLS/HTTPS 加密**，默认启用，保护密码和数据传输
- **密码认证**，Argon2id 哈希，防暴力破解（指数退避）
- **心跳检测**，客户端 5 秒心跳，服务端 30 秒超时
- **UPnP 端口映射**，自动配置外网访问
- **连接数限制**，防止资源耗尽和 DoS 攻击
- **震动反馈**，ViGEmBus 驱动通知转发到手机振动

### 技术栈

| 层级 | 技术 |
|------|------|
| 后端语言 | Rust (Edition 2021) |
| Web 框架 | Axum 0.8 (Tokio 运行时) |
| Web 服务器 | hyper 1 + hyper-util（单连接模式） |
| 虚拟手柄 | vigem-client 0.1.4 (ViGEmBus) |
| TLS/证书 | tokio-rustls 0.26 + rustls 0.23 + rcgen 0.13 |
| 密码哈希 | argon2 0.5 (Argon2id) |
| 序列化 | serde + serde_json + toml |
| UPnP | portmapper 0.19 |
| 日志 | tracing + tracing-subscriber |
| CLI | clap 4 |

---

## 整体架构

### 架构图

```
┌─────────────────────────────────────────────────────────────┐
│                        客户端 (手机浏览器)                        │
│  ┌──────────┐    ┌──────────┐    ┌───────────────────┐    │
│  │ 触屏输入  │───▶│  JS 逻辑 │───▶│  WebSocket (wss)│    │
│  └──────────┘    └──────────┘    └────────┬──────────┘    │
│                                          │                │
└──────────────────────────────────────────┼────────────────┘
                                           │  互联网/局域网
                                           ▼
┌─────────────────────────────────────────────────────────────┐
│                    服务端 (Windows PC)                          │
│                                                            │
│  ┌──────────────────────────────────────────────────────┐  │
│  │              单端口服务器 (hyper + tokio-rustls)         │  │
│  │                                                      │  │
│  │  ┌──────────────────────────────────────────────┐    │  │
│  │  │         协议嗅探 (Protocol Sniffing)          │    │  │
│  │  │  peek 首字节: 0x16=TLS / 其他=HTTP 明文        │    │  │
│  │  └──────┬─────────────────────────────┬───────────┘    │  │
│  │         │ TLS                        │ HTTP 明文        │  │
│  │         ▼                            ▼                 │  │
│  │  ┌──────────────┐          ┌─────────────────────┐     │  │
│  │  │ TlsAcceptor  │          │ enable_tls=true?    │     │  │
│  │  │ (rustls)     │          │  Y: 301→HTTPS       │     │  │
│  │  └──────┬───────┘          │  N: HTTP 服务       │     │  │
│  │         │                  └─────────┬───────────┘     │  │
│  │         └──────────┬─────────────────┘                 │  │
│  │                    ▼                                    │  │
│  │  ┌──────────────────────────────────────────────┐     │  │
│  │  │          Web 模块 (web/)                     │     │  │
│  │  │  - 路由 / 静态页面 / WS 升级                 │     │  │
│  │  │  - 连接限制 / 认证 / 消息处理                │     │  │
│  │  └───────────────┬──────────────────────────────┘     │  │
│  └──────────────────┼──────────────────────────────────┘  │
│                     │                                     │
│  ┌──────────────────▼──────────────────────┐              │
│  │         业务层                             │              │
│  │  ┌─────────┐ ┌─────────┐ ┌──────────┐   │              │
│  │  │  Auth   │ │ Gamepad │ │  UPnP    │   │              │
│  │  │ Manager │ │ Manager │ │  Mapper  │   │              │
│  │  └────┬────┘ └────┬────┘ └─────┬────┘   │              │
│  └───────┼──────────┼────────────┼──────┘              │
│          │          │            │                       │
│  ┌───────▼──────────▼────────────▼──────┐               │
│  │         外部依赖                          │               │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐│               │
│  │  │ Argon2   │ │ ViGEmBus │ │ portmapper││               │
│  │  └──────────┘ └──────────┘ └──────────┘│               │
│  └──────────────────────────────────────────┘               │
└─────────────────────────────────────────────────────────────┘
```

### 分层设计

| 层级 | 职责 | 主要文件 |
|------|------|----------|
| 入口层 | 程序启动、CLI 解析、服务器生命周期 | [main.rs](file:///e:/项目/WebPad/src/main.rs) |
| Web 层 | 协议嗅探、HTTP/WS 服务、路由、连接管理、消息处理 | [web/server.rs](file:///e:/项目/WebPad/src/web/server.rs), [web/handler.rs](file:///e:/项目/WebPad/src/web/handler.rs) |
| 业务层 | 认证、手柄管理、UPnP 映射 | [auth.rs](file:///e:/项目/WebPad/src/auth.rs), [gamepad/](file:///e:/项目/WebPad/src/gamepad/), [upnp/](file:///e:/项目/WebPad/src/upnp/) |
| 协议层 | 消息类型定义、状态转换 | [protocol.rs](file:///e:/项目/WebPad/src/protocol.rs), [gamepad/types.rs](file:///e:/项目/WebPad/src/gamepad/types.rs) |
| 配置层 | 配置加载/保存、默认值、迁移 | [config.rs](file:///e:/项目/WebPad/src/config.rs) |
| 安全层 | 密码哈希与验证 | [password.rs](file:///e:/项目/WebPad/src/password.rs) |

---

## 模块详解

### 1. 入口模块 (main.rs)

**文件**: [main.rs](file:///e:/项目/WebPad/src/main.rs)

**职责**:
- 命令行参数解析（clap）
- 配置文件加载与保存
- TLS Acceptor 构建（自定义证书或自签名证书）
- 单端口服务器启动（协议嗅探模式）
- UPnP 端口映射初始化
- 优雅关闭（broadcast 通道）
- Ctrl-C 信号处理

**关键函数**:

| 函数 | 说明 |
|------|------|
| `main()` | 程序主入口，异步运行时启动 |
| `build_tls_acceptor()` | 构建 TlsAcceptor，优先用户证书，回退自签名 |
| `create_tls_acceptor()` | 从证书链和私钥创建 TlsAcceptor |
| `load_certs()` | 从 PEM 文件加载证书链 |
| `load_key()` | 从 PEM 文件加载私钥 |
| `generate_self_signed_cert()` | 使用 rcgen 生成自签名证书（localhost, 127.0.0.1） |

**启动流程**:
1. 初始化 tracing 日志
2. 解析 CLI 参数
3. 加载/创建配置文件（自动迁移旧版本配置）
4. CLI 参数覆盖配置
5. 确保密码存在（自动生成 16 位随机密码）
6. 创建认证管理器、手柄管理器、UPnP 映射器
7. 构建 TLS Acceptor（如 `enable_tls = true`）
8. 创建 broadcast 通道用于优雅关闭
9. 尝试 UPnP 端口映射
10. 启动单端口服务器（`run_server`），协议嗅探处理 HTTP/HTTPS
11. 等待 Ctrl-C 信号，通过 broadcast 通知服务器关闭

---

### 2. Web 模块 (web/)

#### 2.1 server.rs

**文件**: [web/server.rs](file:///e:/项目/WebPad/src/web/server.rs)

**职责**:
- 路由定义与创建
- 应用共享状态 (`AppState`)
- 连接数限制器 (`ConnectionLimiter`)
- 跨平台手柄句柄封装 (`GamepadHandle`)
- **协议嗅探服务器**（单端口同时处理 HTTP 和 HTTPS）

**关键类型**:

| 类型 | 说明 |
|------|------|
| `AppState` | 应用共享状态，包含 auth、gamepad、upnp、config、connection_limiter |
| `ConnectionLimiter` | 连接数限制器，双 Semaphore（总连接 + 未认证连接） |
| `GamepadHandle` | 跨平台手柄句柄，Windows 上有实际实现，其他平台为空 |

**关键函数**:

| 函数 | 说明 |
|------|------|
| `run_server()` | 单端口服务器主循环，`tokio::select!` 监听新连接和关闭信号 |
| `handle_connection()` | 处理单个 TCP 连接：peek 首字节嗅探协议，分发 TLS/HTTP |
| `create_router()` | 创建 Axum 路由（`/` 首页，`/ws` WebSocket 升级） |
| `security_headers_middleware()` | 添加安全响应头（HSTS、CSP、X-Frame-Options 等） |

**协议嗅探机制**:
- 使用 `TcpStream::peek()` 读取前 5 字节，不消费数据
- TLS ClientHello 检测：首字节 `0x16`（Handshake），第二字节 `0x03`/`0x02`（TLS 版本）
- TLS 连接：`TlsAcceptor.accept()` 升级为 TLS 流，再通过 `hyper::server::conn::http1` 处理
- HTTP 明文 + TLS 启用：返回 `301 Moved Permanently` 重定向到 HTTPS
- HTTP 明文 + TLS 禁用：直接通过 `hyper::server::conn::http1` 以纯 HTTP 模式服务

**ConnectionLimiter 设计**:
- 两个独立的 `tokio::sync::Semaphore`：
  - `total`: 总并发 WebSocket 连接数（默认 8）
  - `unauth`: 未认证并发连接数（默认 3）
- `try_acquire_total()` / `try_acquire_unauth()`: 非阻塞获取 permit
- permit 持有到连接结束，认证成功后释放 unauth permit
- 超出限制返回 503，不进入 WebSocket 握手

**优雅关闭**:
- 使用 `tokio::sync::broadcast` 通道传递关闭信号
- `run_server()` 在 `select!` 中监听 `shutdown.recv()`，收到后退出主循环

**路由表**:

| 路径 | 方法 | 处理器 | 说明 |
|------|------|--------|------|
| `/` | GET | `handler::index` | 返回虚拟手柄 HTML 页面 |
| `/ws` | GET | `handler::ws_upgrade` | WebSocket 升级端点 |

#### 2.2 handler.rs

**文件**: [web/handler.rs](file:///e:/项目/WebPad/src/web/handler.rs)

**职责**:
- HTTP 请求处理（首页、WS 升级）
- WebSocket 消息循环
- 认证流程处理
- 手柄状态转发
- 心跳超时检测
- 震动事件转发

**关键函数**:

| 函数 | 说明 |
|------|------|
| `index()` | 首页处理器，返回内联 HTML |
| `ws_upgrade()` | WebSocket 升级，连接限制检查 |
| `acquire_connection_permits()` | 获取连接 permit（两道门槛） |
| `handle_socket()` | WebSocket 消息处理主循环 |
| `handle_auth_request()` | 处理认证请求，指数退避 |
| `create_controller()` | 创建虚拟手柄控制器 |
| `update_gamepad_state()` | 更新手柄状态到 ViGEmBus |
| `destroy_controller()` | 销毁虚拟手柄控制器 |
| `index_html()` | 生成首页 HTML（内联 CSS/JS） |

**消息处理循环 (`handle_socket`)**:

使用 `tokio::select!` 并发监听三个事件源：

1. **客户端消息** (`receiver.next()`)
   - `AuthRequest`: 密码认证
   - `GamepadState`: 手柄状态更新
   - `Heartbeat`: 心跳，回复 Pong 并重置超时
   - `Disconnect`: 客户端主动断开

2. **震动事件** (`rumble_rx.recv()`)
   - 来自 ViGEmBus 驱动的震动通知
   - 转发为 `ServerMessage::Rumble`

3. **心跳超时** (`sleep(heartbeat_timeout)`)
   - 30 秒未收到心跳则断开连接

**认证流程**:
1. 未认证连接占用 unauth permit
2. 客户端发送 `AuthRequest { password }`
3. 验证密码：
   - 成功：创建 session token，释放 unauth permit，创建虚拟手柄
   - 失败：计数 +1，指数退避延迟，剩余次数提示
4. 超过 `max_auth_failures`（默认 5 次）断开连接

**预认证机制**:
- 连接时 URL 参数 `?token=xxx` 携带 session token
- token 有效则直接标记为已认证，不占用 unauth permit
- 用于断线重连时快速恢复

---

### 3. 认证模块 (auth.rs)

**文件**: [auth.rs](file:///e:/项目/WebPad/src/auth.rs)

**职责**:
- 密码验证
- Session token 管理（创建、验证、移除）
- Session 集合维护

**关键类型**: `AuthManager`

| 方法 | 说明 |
|------|------|
| `new(password_hash)` | 创建认证管理器（需传入密码哈希） |
| `verify_password(password)` | 验证密码（fail-closed，空哈希全部失败） |
| `create_session()` | 创建新 session，返回 32 字节 hex token |
| `validate_token(token)` | 验证 token 是否有效 |
| `remove_session(token)` | 移除 session（连接断开时清理） |
| `session_count()` | 当前活跃 session 数量 |

**设计要点**:
- 使用 `std::sync::Mutex<HashSet<String>>` 存储 session 集合
- 锁持有时间极短（仅插入/查询/删除）
- Token 生成：32 字节随机数，hex 编码（64 字符）
- Fail-closed：密码未配置时所有认证失败

---

### 4. 密码模块 (password.rs)

**文件**: [password.rs](file:///e:/项目/WebPad/src/password.rs)

**职责**:
- 密码哈希生成与验证
- 新旧格式兼容

**关键函数**:

| 函数 | 说明 |
|------|------|
| `hash_password(password)` | 生成 Argon2id 哈希，失败回退 SHA-256 |
| `verify_password(password, hash)` | 验证密码，自动识别格式 |
| `is_old_format(hash)` | 判断是否为旧格式（纯 SHA-256 hex） |

**哈希格式**:
- **新格式**：Argon2id（`$argon2id$...`），64 字符盐
- **旧格式**：纯 SHA-256 hex（64 字符），兼容旧版本
- **空哈希**：fail-closed，所有验证返回 false

**安全特性**:
- Argon2id 默认参数
- 随机盐（每哈希独立）
- 向后兼容 SHA-256 旧格式

---

### 5. 配置模块 (config.rs)

**文件**: [config.rs](file:///e:/项目/WebPad/src/config.rs)

**职责**:
- 配置结构定义与默认值
- 配置文件加载/保存（TOML 格式）
- 配置自动迁移
- 密码生成

**配置项**:

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `port` | u16 | 8443 | 单端口监听端口（同时处理 HTTP/HTTPS） |
| `enable_tls` | bool | true | 是否启用 TLS。设为 false 则以纯 HTTP 模式运行 |
| `http_redirect_port` | u16 | 0 | （已废弃）保留用于向后兼容，不影响运行 |
| `password` | String | "" | 连接密码（明文存储，运行时使用哈希） |
| `cert_path` | Option\<String\> | None | TLS 证书路径（None 则自动生成自签名） |
| `key_path` | Option\<String\> | None | TLS 私钥路径 |
| `enable_upnp` | bool | false | 是否启用 UPnP |
| `heartbeat_timeout_secs` | u64 | 30 | 心跳超时秒数 |
| `max_auth_failures` | u32 | 5 | 最大认证失败次数 |
| `auth_backoff_base_ms` | u64 | 500 | 认证退避初始毫秒 |
| `auth_backoff_max_ms` | u64 | 10000 | 认证退避最大毫秒 |
| `max_connections` | usize | 8 | 最大并发连接数 |
| `max_unauth_connections` | usize | 3 | 最大未认证连接数 |

**配置迁移**:
- **v1 → v2（单端口合并）**：检测 `http_redirect_port > 0 && !enable_tls`，迁移为 `enable_tls = true, http_redirect_port = 0`
- **旧版端口迁移**：检测 `port == 8080 && enable_tls`，迁移为 `port = 8443`
- 迁移后自动保存

**配置文件位置**: 可执行文件同目录下的 `webpad.toml`

---

### 6. 协议模块 (protocol.rs)

**文件**: [protocol.rs](file:///e:/项目/WebPad/src/protocol.rs)

**职责**:
- 定义客户端 → 服务端消息类型
- 定义服务端 → 客户端消息类型
- serde 序列化/反序列化（tag = "type"，snake_case）

#### ClientMessage (客户端 → 服务端):

| 消息类型 | 字段 | 说明 |
|----------|------|------|
| `auth_request` | `password: String` | 认证请求 |
| `gamepad_state` | `buttons: u16` | 按钮位掩码 |
| | `left_trigger: u8` | 左扳机 0-255 |
| | `right_trigger: u8` | 右扳机 0-255 |
| | `thumb_lx: i16` | 左摇杆 X -32768~32767 |
| | `thumb_ly: i16` | 左摇杆 Y |
| | `thumb_rx: i16` | 右摇杆 X |
| | `thumb_ry: i16` | 右摇杆 Y |
| `heartbeat` | - | 心跳包 |
| `disconnect` | `reason: String` | 主动断开 |

#### ServerMessage (服务端 → 客户端):

| 消息类型 | 字段 | 说明 |
|----------|------|------|
| `auth_success` | `token: String` | 认证成功，返回 session token |
| `auth_failure` | `reason: String` | 认证失败原因 |
| `gamepad_state` | 同 ClientMessage | 状态回显（可选） |
| `rumble` | `left_motor: u8` | 左马达震动（低频） |
| | `right_motor: u8` | 右马达震动（高频） |
| `pong` | - | 心跳响应 |
| `connected` | `controller_name: String` | 连接成功通知 |

**按钮位掩码 (buttons u16)**:

| 位 | 按钮 | 常量 |
|----|------|------|
| 0 | 上 (D-Pad) | UP = 1<<0 |
| 1 | 下 (D-Pad) | DOWN = 1<<1 |
| 2 | 左 (D-Pad) | LEFT = 1<<2 |
| 3 | 右 (D-Pad) | RIGHT = 1<<3 |
| 4 | Start | START = 1<<4 |
| 5 | Back | BACK = 1<<5 |
| 6 | 左摇杆按下 | LTHUMB = 1<<6 |
| 7 | 右摇杆按下 | RTHUMB = 1<<7 |
| 8 | LB | LB = 1<<8 |
| 9 | RB | RB = 1<<9 |
| 10 | Guide | GUIDE = 1<<10 |
| 12 | A | A = 1<<12 |
| 13 | B | B = 1<<13 |
| 14 | X | X = 1<<14 |
| 15 | Y | Y = 1<<15 |

---

### 7. 游戏手柄模块 (gamepad/)

#### 7.1 types.rs

**文件**: [gamepad/types.rs](file:///e:/项目/WebPad/src/gamepad/types.rs)

**职责**:
- 业务层手柄状态类型
- 震动事件类型
- 与协议层的转换

**关键类型**:

| 类型 | 说明 |
|------|------|
| `GamepadState` | 手柄状态（buttons、triggers、thumbsticks） |
| `RumbleEvent` | 震动事件（large_motor、small_motor） |

**GamepadState 方法**:
- `from_client_message(msg)`: 从 `ClientMessage` 转换
- `is_zero()`: 是否为全零状态（无输入）
- `merge(other)`: 合并状态（完全覆盖）

#### 7.2 manager.rs (Windows only)

**文件**: [gamepad/manager.rs](file:///e:/项目/WebPad/src/gamepad/manager.rs)

**职责**:
- ViGEmBus 驱动连接管理
- 虚拟 Xbox 360 手柄创建/销毁/状态更新
- 震动通知监听与转发

**关键类型**: `GamepadManager`

| 方法 | 说明 |
|------|------|
| `new()` | 创建管理器并连接 ViGEmBus（失败 panic） |
| `create_controller()` | 创建虚拟手柄，返回 (id, rumble_rx) |
| `update_state(id, state)` | 更新指定手柄的状态 |
| `destroy_controller(id)` | 销毁指定手柄 |
| `shutdown()` | 关闭管理器，销毁所有手柄 |
| `controller_count()` | 当前控制器数量 |

**架构要点**:
- 所有 ViGEm 同步操作通过 `tokio::task::spawn_blocking` 在阻塞线程池执行
- 震动通知：独立 OS 线程从 ViGEmBus 驱动捕获，通过 mpsc 通道转发
- 控制器销毁顺序：先 drop target（触发 unplug），再 join 通知线程
- TargetId: 微软 Xbox 360 控制器 (vendor=0x045E, product=0x028E)

**错误类型**: `GamepadError`
- `ConnectionFailed`: ViGEmBus 连接失败
- `CreateFailed`: 控制器创建失败
- `ControllerNotFound`: 控制器未找到
- `UpdateFailed`: 状态更新失败
- `Shutdown`: 管理器已关闭

---

### 8. UPnP 模块 (upnp/)

**文件**: [upnp/mapper.rs](file:///e:/项目/WebPad/src/upnp/mapper.rs)

**职责**:
- UPnP 端口映射管理
- 外部地址监听
- 端口映射自动续期

**关键类型**: `PortMapper`

| 方法 | 说明 |
|------|------|
| `new(port)` | 创建映射器，后台自动运行 |
| `probe()` | 探测 UPnP 网关可用性 |
| `procure_mapping()` | 请求端口映射 |
| `deactivate()` | 停用端口映射 |
| `current_external_address()` | 当前外部地址（如有） |
| `external_address()` | watch 通道，监听地址变化 |
| `update_port(port)` | 更新本地端口 |

**配置**:
- 仅启用 UPnP（禁用 PCP 和 NAT-PMP）
- TCP 协议
- 后台自动续期映射

---

### 9. 前端 (index_html)

**位置**: [web/handler.rs](file:///e:/项目/WebPad/src/web/handler.rs) 中的 `index_html()` 函数内联

**技术**: 原生 HTML/CSS/JS，无外部依赖

**功能**:
- 虚拟手柄 UI（摇杆、扳机、ABXY、十字键、肩键、Back/Start）
- WebSocket 连接与重连（指数退避，最大 30 秒）
- 密码认证对话框
- 心跳机制（5 秒发送，12 秒超时）
- 触屏与鼠标双支持
- 横竖屏自动旋转（强制横屏显示）
- 响应式缩放
- FPS 显示
- 震动反馈（navigator.vibrate）
- **编辑模式**：自定义手柄布局（拖拽移动、角落手柄调整大小、背景颜色调色板）
- **跨浏览器调色板**：16 色预设色块 + Hex 文本输入 + 颜色预览，替代原生 `<input type="color">`
- **手机竖屏旋转适配**：`clientToInner()` 函数转换视口坐标到内部坐标系

**心跳**:
- 客户端每 5 秒发送一次 `heartbeat` 消息
- 客户端 12 秒未收到 pong 则主动断开重连
- 服务端 30 秒未收到心跳则断开连接

---

## 关键数据结构与协议

### 共享状态 (`AppState`)

```rust
pub struct AppState {
    pub auth: Arc<AuthManager>,
    pub gamepad: GamepadHandle,
    pub upnp: Arc<PortMapper>,
    pub config: Arc<Config>,
    pub connection_limiter: Arc<ConnectionLimiter>,
    pub ip_backoff: Arc<IpBackoffManager>,
}
```

所有字段都包装在 Arc 中，支持多线程安全共享。

### 连接生命周期

```
连接请求
   │
   ▼
acquire_connection_permits()
   │
   ├─ 失败 ──▶ 返回 503
   │
   ▼ 成功
WebSocket 握手
   │
   ▼
handle_socket()
   │
   ├─ 预认证 token 有效 ──▶ 直接认证，创建手柄
   │
   └─ 未认证 ──▶ 等待 AuthRequest
                    │
                    ├─ 认证成功 ──▶ 释放 unauth permit，创建手柄
                    │
                    └─ 认证失败 ──▶ 计数+退避，超限断开
                                          │
                                          ▼
                                     连接断开
                                       │
                                       ▼
                                  清理资源：
                                  - 销毁手柄
                                  - 移除 session
                                  - 释放 permit
```

---

## 核心流程

### 1. 启动流程

详见 [main.rs](file:///e:/项目/WebPad/src/main.rs#L38-L181)

1. CLI 解析 → 配置加载 → 密码生成 → 各管理器初始化 → TLS Acceptor 构建 → 单端口服务器启动

### 2. 连接建立流程

详见 [web/server.rs](file:///e:/项目/WebPad/src/web/server.rs#L119-L215) 和 [web/handler.rs](file:///e:/项目/WebPad/src/web/handler.rs)

1. 客户端访问 `https://host:port/` 加载页面
2. 服务端 `handle_connection()` 协议嗅探，TLS 流量升级为 HTTPS
3. 页面 JS 连接 `wss://host:port/ws`
4. 服务端检查总连接限制 → 检查预认证 token → 检查未认证连接限制
5. WebSocket 握手成功，发送 `connected` 消息
6. 客户端发送 `auth_request` 进行密码认证
7. 认证成功，发送 `auth_success`（带 token），创建虚拟手柄
8. 客户端开始发送 `gamepad_state` 和 `heartbeat`

### 3. 手柄状态同步流程

1. 触屏/鼠标事件 → 更新本地状态
2. requestAnimationFrame 驱动，每 16ms 发送一次 `gamepad_state`
3. 服务端收到后验证已认证 → 调用 `GamepadManager::update_state()`
4. `spawn_blocking` 中调用 ViGEmBus `target.update()`
5. 游戏/系统读取虚拟手柄输入

### 4. 震动反馈流程

1. 游戏发送震动指令到 ViGEmBus 驱动
2. ViGEmBus 通知 vigem-client
3. 通知 OS 线程捕获震动事件，通过 mpsc 通道发送
4. `handle_socket` 的 select! 中接收震动事件
5. 发送 `rumble` 消息到客户端
6. 客户端调用 `navigator.vibrate()` 触发手机振动

### 5. 优雅关闭流程

1. 收到 Ctrl-C 信号
2. 通过 broadcast 通道发送关闭信号到 `run_server`
3. `run_server` 主循环退出，停止接受新连接
4. UPnP 映射 deactivate
5. 各连接断开时清理资源（手柄销毁、session 移除、permit 释放）

---

## 配置说明

### 配置文件示例 (webpad.toml)

```toml
port = 8443
enable_tls = true                 # 设为 false 可禁用 TLS，以纯 HTTP 模式运行
password = "your_password_here"
# cert_path = "path/to/cert.pem"  # 留空则自动生成自签名证书
# key_path = "path/to/key.pem"
enable_upnp = true
heartbeat_timeout_secs = 30
max_auth_failures = 5
auth_backoff_base_ms = 500
auth_backoff_max_ms = 10000
max_connections = 8
max_unauth_connections = 3
```

### 命令行参数

```
WebPad 0.1.0
Virtual gamepad simulator

USAGE:
    webpad.exe [OPTIONS]

OPTIONS:
    -p, --port <PORT>                    监听端口（覆盖配置文件）
    -w, --password <PASSWORD>            连接密码（覆盖配置文件）
        --no-upnp                        禁用 UPnP 端口映射
        --heartbeat-timeout <SECONDS>    心跳超时秒数
    -h, --help                           Print help
    -V, --version                        Print version
```

### TLS 配置

**模式 1：TLS 启用（默认，自签名证书）**
- `enable_tls = true`，不配置 `cert_path`/`key_path`
- 自动生成自签名证书，包含 localhost 和 127.0.0.1
- 浏览器会显示安全警告
- 适合局域网使用

**模式 2：TLS 启用（自定义证书）**
- `enable_tls = true`，配置 `cert_path` 和 `key_path`
- PEM 格式
- 适合生产环境或有域名的情况

**模式 3：TLS 禁用（纯 HTTP）**
- `enable_tls = false`
- 以纯 HTTP 模式运行，不生成证书
- 密码以明文传输，仅适合可信网络环境

> 单端口模式下，启用 TLS 时收到的 HTTP 请求会自动返回 301 重定向到 HTTPS。

---

## 安全机制

### 1. TLS 加密与单端口协议嗅探
- 单端口同时处理 HTTP 和 HTTPS 流量（协议嗅探）
- 默认启用 TLS（`enable_tls = true`），保护密码和数据传输
- 启用 TLS 时，HTTP 请求自动重定向到 HTTPS（301 永久重定向）
- 可通过 `enable_tls = false` 禁用 TLS，以纯 HTTP 模式运行

### 2. 密码认证
- Argon2id 密码哈希
- Fail-closed：未配置密码时所有认证失败
- 最大失败次数限制（默认 5 次）
- 指数退避：`base * 2^(failures-1)`，最大 10 秒

### 3. 连接限制
- 总连接数上限（默认 8）
- 未认证连接数上限（默认 3），防暴力破解
- 超出限制返回 503，不进入 WebSocket 握手

### 4. 心跳检测
- 客户端 5 秒心跳
- 服务端 30 秒超时断开
- 防止僵尸连接占用资源

### 5. Session 管理
- 随机 32 字节 token（64 字符 hex）
- 连接断开时清理 session
- 预认证 token 可绕过未认证连接限制

---

## 依赖关系

### 内部模块依赖

```
main.rs
  ├── config.rs
  │     └── password.rs
  ├── auth.rs
  │     └── password.rs
  ├── gamepad/
  │     ├── types.rs
  │     │     └── protocol.rs
  │     └── manager.rs (Windows)
  │           └── types.rs
  ├── upnp/
  │     └── mapper.rs
  └── web/
        ├── server.rs (run_server, handle_connection, protocol sniffing)
        │     ├── auth.rs
        │     ├── config.rs
        │     └── upnp/mapper.rs
        └── handler.rs
              ├── server.rs (AppState, ConnectionLimiter)
              ├── auth.rs
              ├── config.rs
              ├── protocol.rs
              └── gamepad/types.rs
```

### 外部依赖 (Cargo.toml)

| 依赖 | 版本 | 用途 |
|------|------|------|
| tokio | 1 (full) | 异步运行时 |
| axum | 0.8 (ws) | Web 框架 |
| hyper | 1 | HTTP/1.1 连接处理 |
| hyper-util | 0.1 (tokio, server, service) | hyper 工具（TokioIo、TowerToHyperService） |
| tokio-rustls | 0.26 | 异步 TLS |
| serde | 1 (derive) | 序列化/反序列化 |
| serde_json | 1 | JSON 序列化 |
| vigem-client | 0.1.4 | ViGEmBus 绑定 |
| portmapper | 0.19 | UPnP 端口映射 |
| rustls | 0.23 | TLS 实现 |
| rustls-pemfile | 2 | PEM 证书解析 |
| rcgen | 0.13 | 自签名证书生成 |
| http-body-util | 0.1 | HTTP body 工具 |
| http-body | 1 | HTTP body trait |
| bytes | 1 | 字节缓冲 |
| tracing | 0.1 | 日志框架 |
| tracing-subscriber | 0.3 | 日志订阅者 |
| rand | 0.8 | 随机数生成 |
| sha2 | 0.10 | SHA-256 哈希 |
| hex | 0.4 | Hex 编码 |
| tokio-util | 0.7 | Tokio 工具 |
| futures-util | 0.3 | Future 工具 |
| clap | 4 (derive) | CLI 参数解析 |
| toml | 0.8 | TOML 解析 |
| argon2 | 0.5 | Argon2 密码哈希 |

---

## 构建与运行

### 环境要求

- **操作系统**：Windows 10/11（完整功能），其他平台仅 Web 服务可用）
- **Rust**：stable 工具链（Edition 2021）
- **ViGEmBus 驱动**：Windows 必需，需预先安装

### 构建

```bash
# 调试构建
cargo build

# 发布构建
cargo build --release

# 运行
cargo run --release
```

### 运行

1. 安装 ViGEmBus 驱动（Windows 必需）
2. 运行 `webpad.exe`
3. 控制台显示配置信息和密码
4. 手机浏览器访问 `https://<电脑IP>:8443/`
5. 输入密码连接
6. 开始使用虚拟手柄

### 常见问题

**Q: 浏览器显示安全警告？**
A: 默认使用自签名证书，属于正常现象。点击"高级"→"继续访问"即可。或配置自定义证书。

**Q: 手机无法连接？**
A: 检查防火墙设置，确保 8443 端口开放。确认手机和电脑在同一局域网。

**Q: 手柄没反应？**
A: 确认 ViGEmBus 驱动已安装并运行。在游戏设置中确认控制器为 Xbox 360 控制器。

---

## 测试体系

### 测试文件

| 文件 | 测试内容 |
|------|----------|
| [tests/config_test.rs](file:///e:/项目/WebPad/tests/config_test.rs) | 配置模块测试 |
| [tests/auth_test.rs](file:///e:/项目/WebPad/tests/auth_test.rs) | 认证模块测试 |
| [tests/protocol_test.rs](file:///e:/项目/WebPad/tests/protocol_test.rs) | 协议序列化测试 |
| [tests/gamepad_types_test.rs](file:///e:/项目/WebPad/tests/gamepad_types_test.rs) | 手柄类型测试 |
| [tests/web_test.rs](file:///e:/项目/WebPad/tests/web_test.rs) | Web 模块测试 |

### 运行测试

```bash
# 运行所有测试
cargo test

# 运行特定模块测试
cargo test config

# 显示测试输出
cargo test -- --nocapture
```

### 测试覆盖范围

- 配置：默认值、序列化/反序列化、密码哈希、配置迁移
- 密码：Argon2id 哈希、SHA-256 兼容、空密码 fail-closed
- 认证：session 创建/验证/移除、计数
- 协议：消息序列化/反序列化正确性
- 连接限制：permit 获取/释放、限制触发、预认证绕过
- UPnP：创建、停用、显示格式
- 手柄类型：状态转换、零状态判断

---

## 附录

### 按钮位掩码速查表

```
bit 0:  D-Pad Up
bit 1:  D-Pad Down
bit 2:  D-Pad Left
bit 3:  D-Pad Right
bit 4:  Start
bit 5:  Back / Select
bit 6:  Left Thumb (press)
bit 7:  Right Thumb (press)
bit 8:  Left Bumper (LB)
bit 9:  Right Bumper (RB)
bit 10: Guide
bit 11: (reserved)
bit 12: A
bit 13: B
bit 14: X
bit 15: Y
```

### 目录结构

```
WebPad/
├── src/
│   ├── main.rs              # 程序入口
│   ├── lib.rs               # 库入口（模块导出）
│   ├── config.rs            # 配置管理
│   ├── protocol.rs          # 协议定义
│   ├── auth.rs              # 认证管理
│   ├── password.rs          # 密码哈希
│   ├── gamepad/
│   │   ├── mod.rs           # 手柄模块导出
│   │   ├── types.rs         # 手柄类型定义
│   │   └── manager.rs       # 手柄管理器 (Windows)
│   ├── upnp/
│   │   ├── mod.rs           # UPnP 模块导出
│   │   └── mapper.rs        # UPnP 端口映射
│   └── web/
│       ├── mod.rs           # Web 模块导出
│       ├── server.rs        # 协议嗅探服务器、路由、状态
│       └── handler.rs       # 请求与消息处理
├── tests/                   # 集成测试
├── Cargo.toml               # 项目配置
└── docs/
    └── CODE_WIKI.md         # 本文档
```
