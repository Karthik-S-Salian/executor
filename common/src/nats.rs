use anyhow::{Result, anyhow};
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy, PullConsumer};
use async_nats::jetstream::{self, AckKind, Context, stream::RetentionPolicy, stream::StorageType};
use futures::StreamExt;

pub const SUBMISSION_STREAM: &str = "SUBMISSIONS";
pub const SUBMISSION_SUBJECT: &str = "submission.new";

#[derive(Clone)]
pub struct NatsClient {
    pub js: Context,
}

impl NatsClient {
    pub async fn new(url: &str) -> Result<Self> {
        let connection = async_nats::connect(url).await?;
        let js = jetstream::new(connection);
        Ok(Self { js })
    }

    pub async fn publish(&self, subject: String, data: Vec<u8>) -> Result<()> {
        self.js.publish(subject.clone(), data.into()).await?;
        Ok(())
    }

    /// Creates the work-queue stream if it does not exist. Producer-side setup,
    /// so publishing never depends on a worker having started first.
    pub async fn ensure_stream(&self, stream: &str, subject: &str) -> Result<()> {
        self.js
            .get_or_create_stream(jetstream::stream::Config {
                name: stream.to_string(),
                subjects: vec![subject.to_string()],
                storage: StorageType::File,
                retention: RetentionPolicy::WorkQueue,
                ..Default::default()
            })
            .await?;
        Ok(())
    }
}

/// A pulled message. Ack/nak only after the job is actually handled.
pub struct NatsJob {
    msg: jetstream::Message,
}

impl NatsJob {
    pub fn payload(&self) -> &[u8] {
        &self.msg.payload
    }

    pub async fn ack(&self) -> Result<()> {
        self.msg
            .ack()
            .await
            .map_err(|e| anyhow!("error acknowledging message: {}", e))
    }

    pub async fn nak(&self) -> Result<()> {
        self.msg
            .ack_with(AckKind::Nak(None))
            .await
            .map_err(|e| anyhow!("error nacking message: {}", e))
    }
}

pub struct NatsConsumer {
    consumer: PullConsumer,
}

impl NatsConsumer {
    pub async fn new(
        nats: &NatsClient,
        stream: &str,
        subject: &str,
        durable: &str,
    ) -> Result<Self> {
        nats.ensure_stream(stream, subject).await?;

        let consumer: PullConsumer = nats
            .js
            .get_stream(stream)
            .await?
            .get_or_create_consumer(
                durable,
                jetstream::consumer::pull::Config {
                    durable_name: Some(durable.to_string()),
                    deliver_policy: DeliverPolicy::All,
                    ack_policy: AckPolicy::Explicit,
                    max_ack_pending: 1024,
                    max_deliver: 5,
                    ..Default::default()
                },
            )
            .await?;

        Ok(Self { consumer })
    }

    pub async fn pull(&self) -> Result<Option<NatsJob>> {
        // shortcut: one pull request per job. Batch this if pull overhead ever
        // shows up in throughput.
        let mut messages = self.consumer.messages().await?.take(1);
        match messages.next().await {
            Some(Ok(msg)) => Ok(Some(NatsJob { msg })),
            Some(Err(e)) => Err(anyhow!("error pulling message: {}", e)),
            None => Ok(None),
        }
    }
}
