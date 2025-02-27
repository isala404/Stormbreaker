use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use clap::Parser;
use csv::Writer;
use dashmap::DashMap;
use futures::future::select_all;
use reqwest::{Client, RequestBuilder};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::fs::create_dir_all;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{error, info, warn};
use url::Url;

#[derive(Parser, Debug, Clone)]
#[clap(name = "stormbreaker", about = "HTTP load generator")]
struct Args {
    /// Target URL to send requests to
    #[clap(short, long)]
    url: String,

    /// Number of concurrent requests to maintain
    #[clap(short = 'n', long, default_value = "100")]
    concurrency: usize,

    /// HTTP method to use
    #[clap(short, long, default_value = "GET")]
    method: String,

    /// Request body (for POST, PUT, etc.)
    #[clap(short, long)]
    body: Option<String>,

    /// Headers in format "key:value"
    #[clap(short = 'H', long)]
    headers: Vec<String>,

    /// Maximum number of concurrent connections
    #[clap(short = 'c', long)]
    connections: Option<usize>,

    /// Number of seconds between summary reports
    #[clap(short, long, default_value = "10")]
    summary_interval: u64,

    /// Total duration of the test in seconds (0 means infinite)
    #[clap(short, long, default_value = "60")]
    duration: u64,

    /// Print connection errors
    #[clap(short, long, default_value = "false")]
    print_errors: bool,
}

#[derive(Debug, Clone)]
struct ResponseData {
    id: u64,
    timestamp: DateTime<Utc>,
    status: u16,
    latency_ms: u64,
    headers: String,
    body: String,
}

#[derive(Debug, Clone, Default)]
struct Statistics {
    total_requests: u64,
    successful_requests: u64,
    failed_requests: u64,
    latencies: Vec<u64>,
    start_time: Option<Instant>,
}

impl Statistics {
    fn new() -> Self {
        Self {
            total_requests: 0,
            successful_requests: 0,
            failed_requests: 0,
            latencies: Vec::new(),
            start_time: Some(Instant::now()),
        }
    }

    fn add_response(&mut self, status: u16, latency_ms: u64) {
        self.total_requests += 1;
        if (200..300).contains(&status) {
            self.successful_requests += 1;
        } else {
            self.failed_requests += 1;
        }
        self.latencies.push(latency_ms);
    }

    fn avg_latency(&self) -> f64 {
        if self.latencies.is_empty() {
            return 0.0;
        }
        self.latencies.iter().sum::<u64>() as f64 / self.latencies.len() as f64
    }

    fn p99_latency(&self) -> u64 {
        if self.latencies.is_empty() {
            return 0;
        }
        let mut sorted = self.latencies.clone();
        sorted.sort_unstable();
        let idx = (sorted.len() as f64 * 0.99) as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    fn request_rate(&self) -> f64 {
        if let Some(start) = self.start_time {
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                return self.total_requests as f64 / elapsed;
            }
        }
        0.0
    }
}

struct StormBreaker {
    args: Args,
    client: Client,
    stats: Arc<DashMap<String, Statistics>>,
    csv_writer: Arc<tokio::sync::Mutex<Writer<std::fs::File>>>,
    counter: Arc<std::sync::atomic::AtomicU64>,
}

impl StormBreaker {
    async fn new(args: Args) -> Result<Self> {
        // Create runs directory if it doesn't exist
        create_dir_all("runs").await?;

        // Create a timestamped CSV file
        let timestamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
        let csv_path = PathBuf::from(format!("runs/{}.csv", timestamp));
        let file = std::fs::File::create(&csv_path)
            .with_context(|| format!("Failed to create CSV file: {:?}", csv_path))?;
        
        let mut writer = csv::Writer::from_writer(file);
        writer.write_record([
            "id",
            "timestamp",
            "status",
            "latency_ms",
            "headers",
            "body",
        ])?;
        writer.flush()?;

        // Set up HTTP client with optimized configuration
        let client = Client::builder()
            .pool_max_idle_per_host(args.connections.unwrap_or(50))
            .tcp_keepalive(Duration::from_secs(30))
            .timeout(Duration::from_secs(30))
            .build()?;

        // Configure tracing
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .context("Failed to set tracing subscriber")?;

        Ok(Self {
            args,
            client,
            stats: Arc::new(DashMap::new()),
            csv_writer: Arc::new(tokio::sync::Mutex::new(writer)),
            counter: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    async fn run(&self) -> Result<()> {
        info!("Starting HTTP load generator");
        info!("Target URL: {}", self.args.url);
        info!("Concurrency: {} parallel requests", self.args.concurrency);
        info!("Method: {}", self.args.method);
        info!("Duration: {} seconds", self.args.duration);
        info!("Summary interval: {} seconds", self.args.summary_interval);
        info!("Body: {}", self.args.body.clone().unwrap_or("None".to_string()));
        info!("Headers: {:?}", self.args.headers);
        info!("Connections: {}", self.args.connections.unwrap_or(50));
        
        // Setup statistics tracking
        let (tx, mut rx) = mpsc::channel::<ResponseData>(10000);
        
        // Setup control channel for reporter
        let (stop_tx, mut stop_rx) = mpsc::channel::<bool>(1);
        
        // Start the reporter task
        let stats_clone = self.stats.clone();
        let summary_interval = self.args.summary_interval;
        let reporter_task = tokio::spawn(async move {            
            // Create a window for the last interval and total
            stats_clone.insert("window".to_string(), Statistics::new());
            stats_clone.insert("total".to_string(), Statistics::new());
            
            // Print table header
            println!("{}", "-".repeat(101));
            println!("| {:^10} | {:^10} | {:^10} | {:^10} | {:^12} | {:^12} | {:^15} |", 
                "Interval", "Requests", "Success", "Failed", "Avg Latency", "P99 Latency", "Req/sec");
            println!("{}", "-".repeat(101));
            
            let mut intervals = 0;
            
            loop {
                tokio::select! {
                    _ = sleep(Duration::from_secs(summary_interval)) => {
                        // Print the report for this interval
                        intervals += 1;
                        if stats_clone.get("total").is_some() {
                            if let Some(window_stats) = stats_clone.get("window") {
                                let window = window_stats.clone();
                                println!("| {:^10} | {:^10} | {:^10} | {:^10} | {:^12.2} | {:^12} | {:^15.2} |", 
                                    intervals,
                                    window.total_requests,
                                    window.successful_requests,
                                    window.failed_requests,
                                    window.avg_latency(),
                                    window.p99_latency(),
                                    window.request_rate());
                            }
                        }
                        
                        // Reset the window statistics AFTER reporting
                        stats_clone.insert("window".to_string(), Statistics::new());
                    }
                    _ = stop_rx.recv() => {
                        // Final report on exit
                        println!("{}", "-".repeat(101));
                        if let Some(total_stats) = stats_clone.get("total") {
                            let total = total_stats.clone();
                            println!("| {:^10} | {:^10} | {:^10} | {:^10} | {:^12.2} | {:^12} | {:^15.2} |", 
                                "TOTAL",
                                total.total_requests,
                                total.successful_requests,
                                total.failed_requests,
                                total.avg_latency(),
                                total.p99_latency(),
                                total.request_rate());
                        }
                        println!("{}", "-".repeat(101));
                        // exit 0
                        std::process::exit(0);
                    }
                }
            }
        });

        // Start the CSV writer task
        let csv_writer = self.csv_writer.clone();
        let writer_task = tokio::spawn(async move {
            while let Some(response) = rx.recv().await {
                let mut writer = csv_writer.lock().await;
                if let Err(e) = writer.write_record([
                    response.id.to_string(),
                    response.timestamp.to_rfc3339(),
                    response.status.to_string(),
                    response.latency_ms.to_string(),
                    response.headers,
                    response.body,
                ]) {
                    error!("Failed to write to CSV: {}", e);
                }
                if let Err(e) = writer.flush() {
                    error!("Failed to flush CSV writer: {}", e);
                }
            }
        });

        // Set up the request generator
        let counter = self.counter.clone();
        let stats = self.stats.clone();
        let tx_clone = tx.clone();
        let args_clone = self.args.clone();
        let client_clone = self.client.clone();

        // Calculate test end time
        let end_time = if self.args.duration > 0 {
            Some(Instant::now() + Duration::from_secs(self.args.duration))
        } else {
            None
        };

        let mut tasks = Vec::with_capacity(self.args.concurrency);

        // Initial batch - start up to concurrency level of tasks
        for _ in 0..self.args.concurrency {
            let req_id = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let tx = tx_clone.clone();
            let args = args_clone.clone();
            let client = client_clone.clone();
            let stats = stats.clone();
            
            let task = tokio::spawn(async move {
                if let Err(e) = Self::send_request(req_id, &args, client, tx, stats).await {
                    if args.print_errors {
                        error!("Request failed: {}", e);
                    }
                }
            });
            
            tasks.push(task);
        }

        // Main loop - maintain constant concurrency
        loop {
            // Check if test duration has elapsed
            if let Some(end_time) = end_time {
                if Instant::now() >= end_time {
                    break;
                }
            }
            
            if tasks.is_empty() {
                break;
            }
            
            // Create references to the tasks
            let mut task_refs: Vec<_> = tasks.iter_mut().collect();
            
            // Wait for any task to complete
            let (result, index, _) = select_all(&mut task_refs).await;
            
            // Handle any errors from the completed task
            if let Err(e) = result {
                error!("Task failed: {}", e);
            }
            
            // Remove the completed task
            tasks.remove(index);
            
            // Create a new task to maintain concurrency
            let req_id = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let tx = tx_clone.clone();
            let args = args_clone.clone();
            let client = client_clone.clone();
            let stats = stats.clone();
            
            let task = tokio::spawn(async move {
                if let Err(e) = Self::send_request(req_id, &args, client, tx, stats).await {
                    error!("Request failed: {}", e);
                }
            });
            
            tasks.push(task);
        }

        // Signal to the reporter to print the final stats and exit
        let _ = stop_tx.send(true).await;
        
        // Wait for all tasks to complete or cancel them
        for task in tasks {
            task.abort();
        }
        
        // Ensure all data is written
        drop(tx);
        if let Err(e) = writer_task.await {
            error!("Error in writer task: {}", e);
        }
        
        // Wait for the reporter task to complete
        if let Err(e) = reporter_task.await {
            error!("Error in reporter task: {}", e);
        }
        
        Ok(())
    }

    async fn send_request(
        id: u64,
        args: &Args,
        client: Client,
        tx: mpsc::Sender<ResponseData>,
        stats: Arc<DashMap<String, Statistics>>,
    ) -> Result<()> {
        // Parse URL
        let url = Url::parse(&args.url)?;
        
        // Create request builder with appropriate method
        let req_builder = match args.method.to_uppercase().as_str() {
            "GET" => client.get(url),
            "POST" => client.post(url),
            "PUT" => client.put(url),
            "DELETE" => client.delete(url),
            "HEAD" => client.head(url),
            "PATCH" => client.patch(url),
            other => {
                warn!("Unsupported HTTP method '{}', falling back to GET", other);
                client.get(url)
            }
        };
        
        // Add headers and body
        let req_builder = Self::build_request(req_builder, args);
        
        // Record start time
        let start = Instant::now();
        let timestamp = Utc::now();
        
        // Send request
        let resp = req_builder.send().await?;
        
        // Calculate latency
        let latency = start.elapsed();
        let latency_ms = latency.as_millis() as u64;
        
        // Extract status and headers
        let status = resp.status();
        let headers_str = format!("{:?}", resp.headers());
        
        // Extract body
        let body_bytes = resp.bytes().await?;
        let body_str = match String::from_utf8(body_bytes.to_vec()) {
            Ok(s) => s,
            Err(_) => String::from("[binary data]"),
        };
        
        // Create response data
        let response_data = ResponseData {
            id,
            timestamp,
            status: status.as_u16(),
            latency_ms,
            headers: headers_str,
            body: body_str,
        };
        
        // Update statistics
        stats.entry("total".to_string()).and_modify(|stats| {
            stats.add_response(status.as_u16(), latency_ms);
        }).or_insert_with(|| {
            let mut new_stats = Statistics::new();
            new_stats.add_response(status.as_u16(), latency_ms);
            new_stats
        });
        
        stats.entry("window".to_string()).and_modify(|stats| {
            stats.add_response(status.as_u16(), latency_ms);
        }).or_insert_with(|| {
            let mut new_stats = Statistics::new();
            new_stats.add_response(status.as_u16(), latency_ms);
            new_stats
        });
        
        // Send response data to CSV writer
        tx.send(response_data).await?;
        
        Ok(())
    }
    
    fn build_request(mut req_builder: RequestBuilder, args: &Args) -> RequestBuilder {
        // Add headers
        for header in &args.headers {
            if let Some((key, value)) = header.split_once(':') {
                req_builder = req_builder.header(key.trim(), value.trim());
            }
        }
        
        // Add body if provided
        if let Some(body) = &args.body {
            req_builder = req_builder.body(body.clone());
        }
        
        req_builder
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let stormbreaker = StormBreaker::new(args).await?;
    stormbreaker.run().await?;
    Ok(())
}
