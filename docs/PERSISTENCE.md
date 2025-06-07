# Lampo Persistence System

Lampo provides a flexible persistence abstraction that allows you to store LDK channel monitors, channel manager state, and other persistent data using different storage backends.

## Overview

The persistence system is built around the `Persister` trait in `lampo-common`, which provides a generic interface for key-value storage operations. This allows Lampo to support multiple storage backends while maintaining a consistent API.

## Supported Backends

### 1. Filesystem Persistence (Default)

The filesystem backend stores data locally using LDK's `FilesystemStore`. This is the default and most tested option.

**Configuration:**
```toml
persistence-backend=filesystem
```

**Features:**
- Local file storage
- No external dependencies
- Fast read/write operations
- Automatic directory creation

### 2. VSS (Versioned Storage Service) Persistence

The VSS backend provides cloud-based storage with versioning capabilities, enabling recovery and multi-device access.

**Configuration:**
```toml
persistence-backend=vss
vss-endpoint=https://your-vss-server.com
vss-store-id=your-store-id
vss-auth-token=your-auth-token
```

**Features:**
- Cloud-based storage
- Multi-device synchronization
- Versioned data (recovery capabilities)
- Optional client-side encryption
- HTTP-based API

### 3. Database Persistence (Future)

Support for SQL databases is planned for future releases.

## Configuration

### Basic Configuration

Add persistence settings to your `lampo.conf` file:

```toml
# Choose your persistence backend
persistence-backend=filesystem  # or "vss"

# VSS-specific configuration (only needed for VSS backend)
vss-endpoint=https://vss.example.com
vss-store-id=my-lightning-node
vss-auth-token=your-secret-token
```

### Environment Variables

You can also configure persistence using environment variables:

```bash
export LAMPO_PERSISTENCE_BACKEND=vss
export LAMPO_VSS_ENDPOINT=https://vss.example.com
export LAMPO_VSS_STORE_ID=my-node
export LAMPO_VSS_AUTH_TOKEN=secret
```

## Usage Examples

### Using the Persistence Factory

```rust
use lampod::persistence::{PersistenceFactory, VSSConfig};
use std::collections::HashMap;

// Create filesystem persister
let fs_persister = PersistenceFactory::filesystem("/path/to/storage");

// Create VSS persister
let vss_config = VSSConfig {
    endpoint: "https://vss.example.com".to_string(),
    store_id: "my-node".to_string(),
    auth_token: Some("token".to_string()),
    headers: HashMap::new(),
    encryption_key: None,
};
let vss_persister = PersistenceFactory::vss(vss_config);

// Create LDK adapter for any persister
let ldk_adapter = PersistenceFactory::ldk_adapter(fs_persister);
```

### Direct Usage

```rust
use lampo_common::persist::Persister;
use lampod::persistence::FilesystemPersister;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let persister = FilesystemPersister::new("/tmp/lampo-data");

    // Initialize the persister
    persister.initialize().await?;

    // Store some data
    persister.write("my-key", b"Hello, World!").await?;

    // Read it back
    let data = persister.read("my-key").await?;
    println!("Data: {}", String::from_utf8_lossy(&data));

    // Check if key exists
    if persister.exists("my-key").await? {
        println!("Key exists!");
    }

    // List keys with prefix
    let keys = persister.list("my-").await?;
    println!("Found {} keys", keys.len());

    // Clean up
    persister.remove("my-key").await?;
    persister.shutdown().await?;

    Ok(())
}
```

## VSS Integration

### Setting up VSS Server

To use VSS persistence, you need a running VSS server. You can:

1. **Use a hosted VSS service** (when available)
2. **Run your own VSS server** using the [official VSS implementation](https://github.com/lightningdevkit/vss-server)

### VSS Configuration Options

```rust
use lampod::persistence::VSSConfig;
use std::collections::HashMap;

let config = VSSConfig {
    endpoint: "https://vss.example.com".to_string(),
    store_id: "unique-node-identifier".to_string(),
    auth_token: Some("your-auth-token".to_string()),
    headers: {
        let mut headers = HashMap::new();
        headers.insert("X-Custom-Header".to_string(), "value".to_string());
        headers
    },
    encryption_key: Some(derive_key_from_seed(&wallet_seed)), // Optional client-side encryption
};
```

### Security Considerations

- **Authentication**: Always use authentication tokens for VSS
- **Encryption**: Enable client-side encryption for sensitive data
- **HTTPS**: Always use HTTPS endpoints for VSS
- **Key Derivation**: Derive encryption keys from your wallet seed

## LDK Integration

The persistence system integrates seamlessly with LDK through the `LDKPersisterAdapter`:

```rust
use lampod::persistence::{PersistenceFactory, LDKPersisterAdapter};
use lightning::util::persist::KVStore;

// Create any persister
let persister = PersistenceFactory::filesystem("/path/to/data");

// Create LDK adapter
let ldk_adapter = LDKPersisterAdapter::new(persister);

// Use with LDK components
let chain_monitor = ChainMonitor::new(
    None,
    broadcaster,
    logger,
    fee_estimator,
    Arc::new(ldk_adapter), // Implements KVStore
);
```

## Migration Between Backends

You can migrate data between different persistence backends:

```rust
async fn migrate_data(
    source: Arc<dyn Persister>,
    target: Arc<dyn Persister>,
) -> Result<(), Box<dyn std::error::Error>> {
    // List all keys in source
    let keys = source.list("").await?;

    // Copy each key to target
    for key in keys {
        let data = source.read(&key).await?;
        target.write(&key, &data).await?;
    }

    // Sync target
    target.sync().await?;

    Ok(())
}
```

## Error Handling

The persistence system uses Lampo's error handling:

```rust
use lampo_common::error;

match persister.read("non-existent-key").await {
    Ok(data) => println!("Data: {:?}", data),
    Err(e) => {
        if e.to_string().contains("not found") {
            println!("Key doesn't exist");
        } else {
            eprintln!("Error: {}", e);
        }
    }
}
```

## Performance Considerations

### Filesystem Backend
- **Pros**: Fast local access, no network latency
- **Cons**: No backup/recovery, single device only

### VSS Backend
- **Pros**: Cloud backup, multi-device sync, versioning
- **Cons**: Network latency, requires internet connection

### Optimization Tips

1. **Caching**: VSS persister includes automatic caching
2. **Batching**: Group multiple operations when possible
3. **Async**: All operations are async for better performance
4. **Compression**: Consider compressing large data before storage

## Troubleshooting

### Common Issues

1. **VSS Connection Failures**
   ```
   Error: VSS health check failed with status: 404
   ```
   - Check VSS endpoint URL
   - Verify VSS server is running
   - Check network connectivity

2. **Authentication Errors**
   ```
   Error: VSS write failed with status: 401
   ```
   - Verify auth token is correct
   - Check token hasn't expired

3. **Filesystem Permission Errors**
   ```
   Error: Failed to create storage directory: Permission denied
   ```
   - Check directory permissions
   - Ensure parent directories exist

### Debug Logging

Enable debug logging to troubleshoot persistence issues:

```bash
RUST_LOG=lampod::persistence=debug cargo run
```

## Future Enhancements

- **Database Support**: PostgreSQL, SQLite, etc.
- **Encryption**: Built-in client-side encryption
- **Compression**: Automatic data compression
- **Replication**: Multi-backend replication
- **Backup**: Automated backup strategies
- **Monitoring**: Persistence metrics and health checks

## Contributing

To add a new persistence backend:

1. Implement the `Persister` trait
2. Add configuration options to `LampoConf`
3. Update the `PersistenceFactory`
4. Add tests and documentation
5. Submit a pull request

See the existing implementations in `lampod/src/persistence/` for examples.