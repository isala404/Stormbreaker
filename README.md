# StormBreaker - High-Performance HTTP Load Generator

StormBreaker is a highly optimized HTTP load generator written in Rust. It's designed to generate maximum throughput and accurately measure performance metrics.

## Features

- Configurable concurrency level (number of parallel requests)
- Support for different HTTP methods (GET, POST, PUT, DELETE, etc.)
- Customizable request headers and bodies
- Performance monitoring with real-time statistics
- CSV output of all responses with detailed metrics
- Auto-configuration of connection pooling for optimal performance

## Installation

### Prerequisites

- Rust 1.67 or higher

### Building from source

```bash
git clone https://github.com/yourusername/stormbreaker.git
cd stormbreaker
cargo build --release
```

The binary will be available at `target/release/stormbreaker`.

## Usage

```bash
# Basic usage (100 parallel requests for 60 seconds)
stormbreaker --url https://example.com

# Custom concurrency level
stormbreaker --url https://example.com --concurrency 500

# Using POST with a request body
stormbreaker --url https://api.example.com/data --method POST --body '{"key": "value"}'

# Adding custom headers
stormbreaker --url https://example.com --headers "Authorization: Bearer token" --headers "Content-Type: application/json"

# Running for a specific duration
stormbreaker --url https://example.com --duration 300  # 5 minutes

# Custom summary interval
stormbreaker --url https://example.com --summary-interval 5  # Print summary every 5 seconds
```

## Command-line Options

| Option | Description | Default |
|--------|-------------|---------|
| `--url`, `-u` | Target URL to send requests to | (required) |
| `--concurrency`, `-n` | Number of concurrent requests to maintain | 100 |
| `--method`, `-m` | HTTP method | GET |
| `--body`, `-b` | Request body for POST, PUT, etc. | None |
| `--headers`, `-H` | Headers in "key:value" format | None |
| `--connections`, `-c` | Maximum number of connections | Auto |
| `--summary-interval`, `-s` | Seconds between summary reports | 10 |
| `--duration`, `-d` | Total duration in seconds (0 = infinite) | 60 |

## Output

StormBreaker creates a CSV file in the `runs/` directory with a timestamp-based name. Each row in the CSV contains:

- Request ID
- Timestamp
- Status code
- Latency (ms)
- Response headers
- Response body

Additionally, StormBreaker prints summary statistics to the console at regular intervals, showing:

- Total requests
- Successful requests
- Failed requests
- Average latency
- P99 latency
- Request rate

## Performance Optimization

StormBreaker is designed for high performance. It:

- Uses asynchronous I/O with Tokio
- Maintains a constant level of concurrent requests
- Optimizes connection pooling
- Minimizes memory allocations
- Uses efficient data structures for statistics calculation

## License

MIT 
