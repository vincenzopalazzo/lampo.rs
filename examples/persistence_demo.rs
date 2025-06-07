//! Persistence Demo
//!
//! This example demonstrates how to use Lampo's generic persistence
//! abstraction with different backends (filesystem and VSS).

use std::collections::HashMap;
use std::sync::Arc;

use lampo_common::persist::{Persister, PersistenceKind};
use lampod::persistence::{FilesystemPersister, VSSPersister, VSSConfig, PersistenceFactory};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    env_logger::init();

    println!("=== Lampo Persistence Demo ===\n");

    // Demo 1: Filesystem Persistence
    println!("1. Testing Filesystem Persistence");
    demo_filesystem_persistence().await?;

    // Demo 2: VSS Persistence (mock)
    println!("\n2. Testing VSS Persistence (mock)");
    demo_vss_persistence().await?;

    // Demo 3: Using the Factory
    println!("\n3. Testing Persistence Factory");
    demo_persistence_factory().await?;

    println!("\n=== Demo Complete ===");
    Ok(())
}

async fn demo_filesystem_persistence() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let persister = FilesystemPersister::new(temp_dir.path());

    println!("  Persistence kind: {:?}", persister.kind());

    // Initialize
    persister.initialize().await?;
    println!("  ✓ Initialized filesystem persister");

    // Write some data
    let test_data = b"Hello, Lampo persistence!";
    persister.write("test_key", test_data).await?;
    println!("  ✓ Wrote test data");

    // Read it back
    let read_data = persister.read("test_key").await?;
    assert_eq!(test_data, &read_data[..]);
    println!("  ✓ Read test data successfully");

    // Check if key exists
    let exists = persister.exists("test_key").await?;
    assert!(exists);
    println!("  ✓ Key exists check passed");

    // List keys
    let keys = persister.list("test").await?;
    println!("  ✓ Found {} keys with prefix 'test'", keys.len());

    // Remove the key
    persister.remove("test_key").await?;
    println!("  ✓ Removed test key");

    // Verify it's gone
    let exists = persister.exists("test_key").await?;
    assert!(!exists);
    println!("  ✓ Key removal verified");

    // Shutdown
    persister.shutdown().await?;
    println!("  ✓ Shutdown completed");

    Ok(())
}

async fn demo_vss_persistence() -> Result<(), Box<dyn std::error::Error>> {
    // Create a mock VSS configuration
    let config = VSSConfig {
        endpoint: "https://mock-vss.example.com".to_string(),
        store_id: "demo-store".to_string(),
        auth_token: Some("demo-token".to_string()),
        headers: HashMap::new(),
        encryption_key: Some([42u8; 32]), // Mock encryption key
    };

    let persister = VSSPersister::new(config);
    println!("  Persistence kind: {:?}", persister.kind());

    // Note: This will fail because we're using a mock endpoint
    // In a real scenario, you'd have a running VSS server
    match persister.initialize().await {
        Ok(_) => println!("  ✓ VSS persister initialized (unexpected success with mock endpoint)"),
        Err(e) => println!("  ✗ VSS persister failed to initialize (expected with mock): {}", e),
    }

    println!("  ℹ VSS persistence would work with a real VSS server endpoint");

    Ok(())
}

async fn demo_persistence_factory() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;

    // Create filesystem persister using factory
    let fs_persister = PersistenceFactory::filesystem(temp_dir.path());
    println!("  Created filesystem persister via factory");

    // Initialize and test
    fs_persister.initialize().await?;
    fs_persister.write("factory_test", b"Factory works!").await?;
    let data = fs_persister.read("factory_test").await?;
    println!("  ✓ Factory-created persister works: {}", String::from_utf8_lossy(&data));

    // Create VSS persister using factory
    let vss_config = VSSConfig {
        endpoint: "https://mock-vss.example.com".to_string(),
        store_id: "factory-demo".to_string(),
        auth_token: None,
        headers: HashMap::new(),
        encryption_key: None,
    };
    let _vss_persister = PersistenceFactory::vss(vss_config);
    println!("  ✓ Created VSS persister via factory");

    // Create LDK adapter
    let _ldk_adapter = PersistenceFactory::ldk_adapter(fs_persister.clone());
    println!("  ✓ Created LDK adapter via factory");

    Ok(())
}