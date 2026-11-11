// =============================================================================
//    Copyright (c) 2025 - 2026 Haixing Hu.
//
//    SPDX-License-Identifier: Apache-2.0
//
//    Licensed under the Apache License, Version 2.0.
// =============================================================================

#[cfg(not(feature = "async-wait"))]
fn main() {
    eprintln!("Run with `cargo run --example conversion_service --features async-wait`.");
}

#[cfg(feature = "async-wait")]
mod enabled {
    use std::error::Error;
    use std::fmt;
    use std::io;
    use std::sync::mpsc;

    use qubit_executor::SubmissionError;
    use qubit_executor::TaskExecutionError;
    use qubit_executor::TaskHandle;
    use qubit_executor::service::ExecutorService;
    use qubit_thread_pool::ThreadPool;

    type ConversionResult = Result<Vec<u8>, io::Error>;

    #[derive(Debug)]
    enum ServiceError {
        Shutdown,
        Saturated,
        WorkerSpawnFailed(std::sync::Arc<io::Error>),
        OtherSubmission(SubmissionError),
        Task(TaskExecutionError<io::Error>),
        Conversion(io::Error),
        Wait(tokio::task::JoinError),
    }

    impl fmt::Display for ServiceError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Shutdown => f.write_str("service is shutting down"),
                Self::Saturated => f.write_str("service is overloaded"),
                Self::WorkerSpawnFailed(error) => write!(f, "worker creation failed: {error}"),
                Self::OtherSubmission(error) => write!(f, "submission failed: {error}"),
                Self::Task(error) => write!(f, "conversion task failed: {error}"),
                Self::Conversion(error) => write!(f, "conversion failed: {error}"),
                Self::Wait(error) => write!(f, "result bridge failed: {error}"),
            }
        }
    }

    impl Error for ServiceError {}

    struct ConversionService {
        pool: ThreadPool,
    }

    impl ConversionService {
        fn new() -> Result<Self, Box<dyn Error>> {
            let pool = ThreadPool::builder()
                .core_pool_size(1)
                .maximum_pool_size(1)
                .queue_capacity(1)
                .thread_name_prefix("conversion-worker")
                .build()?;
            Ok(Self { pool })
        }

        fn submit(&self, input: Vec<u8>) -> Result<TaskHandle<Vec<u8>, io::Error>, ServiceError> {
            let mut input = Some(input);
            self.pool
                .submit_callable(move || convert(input.take().expect("callable runs once")))
                .map_err(|error| match error {
                    SubmissionError::Saturated => ServiceError::Saturated,
                    SubmissionError::Shutdown => ServiceError::Shutdown,
                    SubmissionError::WorkerSpawnFailed { source } => ServiceError::WorkerSpawnFailed(source),
                    other => ServiceError::OtherSubmission(other),
                })
        }

        async fn convert_request(&self, input: Vec<u8>) -> Result<Vec<u8>, ServiceError> {
            let handle = self.submit(input)?;
            match tokio::task::spawn_blocking(move || handle.get())
                .await
                .map_err(ServiceError::Wait)?
            {
                Ok(output) => Ok(output),
                Err(TaskExecutionError::Failed(error)) => Err(ServiceError::Conversion(error)),
                Err(error) => Err(ServiceError::Task(error)),
            }
        }
    }

    fn convert(input: Vec<u8>) -> ConversionResult {
        let text = String::from_utf8(input).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(text.to_uppercase().into_bytes())
    }

    struct ReleaseGuard(Option<mpsc::Sender<()>>);

    impl ReleaseGuard {
        fn release(&mut self) -> Result<(), Box<dyn Error>> {
            if let Some(sender) = self.0.take() {
                sender.send(())?;
            }
            Ok(())
        }
    }

    impl Drop for ReleaseGuard {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    async fn run() -> Result<(), Box<dyn Error>> {
        let service = ConversionService::new()?;
        let result = run_workflow(&service).await;
        service.pool.shutdown();
        service.pool.await_termination().await;
        result
    }

    async fn run_workflow(service: &ConversionService) -> Result<(), Box<dyn Error>> {
        let output = service.convert_request(b"hello, Qubit".to_vec()).await?;
        assert_eq!(output, b"HELLO, QUBIT");
        println!("converted: {}", String::from_utf8(output)?);

        match service.convert_request(vec![0xff]).await {
            Err(ServiceError::Conversion(error)) => println!("conversion error: {error}"),
            _ => return Err("expected invalid UTF-8 conversion error".into()),
        }

        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut release = ReleaseGuard(Some(release_tx));
        let blocker = service.pool.submit_callable(move || {
            started_tx.send(()).map_err(io::Error::other)?;
            release_rx.recv().map_err(io::Error::other)?;
            Ok::<(), io::Error>(())
        })?;
        started_rx.recv()?;
        let queued = service.submit(b"queued".to_vec())?;
        match service.submit(b"overflow".to_vec()) {
            Err(ServiceError::Saturated) => {
                println!("saturated: excess conversion rejected immediately")
            }
            _ => return Err("expected saturated submission".into()),
        }
        release.release()?;
        match blocker.get() {
            Ok(()) => {}
            Err(TaskExecutionError::Failed(error)) => return Err(error.into()),
            Err(error) => return Err(io::Error::other(error).into()),
        }
        match queued.get() {
            Ok(_) => {}
            Err(TaskExecutionError::Failed(error)) => return Err(error.into()),
            Err(error) => return Err(io::Error::other(error).into()),
        }
        service.pool.shutdown();
        service.pool.await_termination().await;
        match service.submit(b"after shutdown".to_vec()) {
            Err(ServiceError::Shutdown) => println!("shutdown: new conversion rejected"),
            _ => return Err("expected shutdown rejection".into()),
        }
        Ok(())
    }

    #[tokio::main]
    pub async fn main() -> Result<(), Box<dyn Error>> {
        run().await
    }
}

#[cfg(feature = "async-wait")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    enabled::main()
}
