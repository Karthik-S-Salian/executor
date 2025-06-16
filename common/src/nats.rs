use anyhow::{Result, anyhow};
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy, PullConsumer};
use async_nats::jetstream::{self, Context, stream::RetentionPolicy, stream::StorageType};
use futures::StreamExt;

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
}

pub struct JobQueue {
    js: Context,
    stream: String,
    subject: String,
    consumer: PullConsumer,
}

impl JobQueue {
    pub async fn new(
        nats: &NatsClient,
        stream: &str,
        subject: &str,
        durable: &str,
    ) -> Result<Self> {
        let consumer: PullConsumer = nats
            .js
            .get_or_create_stream(jetstream::stream::Config {
                name: stream.to_string(),
                subjects: vec![subject.to_string()],
                storage: StorageType::Memory,
                retention: RetentionPolicy::WorkQueue,
                ..Default::default()
            })
            .await?
            .create_consumer(jetstream::consumer::pull::Config {
                durable_name: Some(durable.to_string()),
                deliver_policy: DeliverPolicy::All,
                ack_policy: AckPolicy::Explicit,
                max_ack_pending: 1,
                ..Default::default()
            })
            .await?;

        Ok(Self {
            js: nats.js.clone(),
            stream: stream.to_string(),
            subject: subject.to_string(),
            consumer,
        })
    }

    pub async fn publish(&self, data: Vec<u8>) -> Result<()> {
        self.js.publish(self.subject.clone(), data.into()).await?;
        Ok(())
    }

    pub async fn pull(&self) -> Result<Option<Vec<u8>>> {
        let mut messages = self.consumer.messages().await?.take(1);
        if let Some(Ok(msg)) = messages.next().await {
            let payload = msg.payload.to_vec();
            let _ = msg
                .ack()
                .await
                .map_err(|e| anyhow!("error acknowledging message: {}", e))?;
            Ok(Some(payload))
        } else {
            Ok(None)
        }
    }
}
