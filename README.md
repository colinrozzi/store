# Key-Value Store Actor

This actor implements a persistent key-value store with support for atomic operations and content-addressed storage. It's designed to be a foundational component that other actors can build upon for their storage needs.

## Core Concepts

### Storage Model
The key-value store provides:
- Simple key-value pair storage
- Content-addressed storage using SHA1 hashes
- Atomic operations for data consistency
- Persistence to disk
- Efficient in-memory caching

### Data Storage
Data is stored in a flat structure:
- All data is serialized to JSON
- Files are stored in the `data/` directory
- Keys can be arbitrary strings
- Values can be any JSON-serializable data

This design:
- Provides simple, reliable storage
- Makes backup and synchronization straightforward
- Enables easy inspection and debugging
- Supports future enhancements like replication

## Key Features

### Persistence
- All data is automatically persisted to disk
- Atomic write operations ensure data consistency
- State can be rebuilt from disk on actor restart

### Content Addressing
- Optional content-addressed storage using SHA1 hashes
- Natural deduplication of identical content
- Content integrity verification
- Efficient caching of frequently accessed data

### Simple Interface
- Basic key-value operations (get, put, delete)
- Support for both direct key access and content addressing
- Built-in support for JSON serialization
- Clear error handling

## Event Types

### `put`
Store a value with a given key:
```json
{
  \"type\": \"put\",
  \"key\": \"some_key\",
  \"value\": \"any json value\"
}
```

### `get`
Retrieve a value by key:
```json
{
  \"type\": \"get\",
  \"key\": \"some_key\"
}
```

### `delete`
Remove a value:
```json
{
  \"type\": \"delete\",
  \"key\": \"some_key\"
}
```

### `store_content`
Store content-addressed data:
```json
{
  \"type\": \"store_content\",
  \"content\": \"any json value\"
}
```

### `get_content`
Retrieve content by hash:
```json
{
  \"type\": \"get_content\",
  \"hash\": \"sha1_hash\"
}
```

## Configuration

The actor is configured via `actor.toml`:
- Implements the `ntwk:actor/actor` interface
- Requires filesystem access for persistence
- Data directory is configured in the filesystem handler

## Why These Design Choices?

### Why a Flat Key-Value Store?
1. **Simplicity**: Easy to understand and implement correctly
2. **Flexibility**: Can support many different usage patterns
3. **Performance**: Direct key lookup is fast and predictable
4. **Reliability**: Fewer moving parts means fewer failure modes

### Why Content Addressing?
1. **Deduplication**: Identical content is stored only once
2. **Integrity**: Easy to verify content hasn't been modified
3. **Caching**: Content-based addressing enables efficient caching
4. **Distribution**: Enables future distributed features

## Future Possibilities

The current design enables several future enhancements:
1. Replication across multiple instances
2. Advanced caching strategies
3. Backup and restore functionality
4. Content validation and type checking
5. Pub/sub notifications for changes
6. Query and filtering capabilities

## Development

Built using:
- Rust for reliability and performance
- WebAssembly for portability
- Serde for serialization
- SHA1 for content addressing

The actor is compiled to WebAssembly and can run in any WASM runtime that implements the required interfaces.
