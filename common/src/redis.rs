use anyhow::Result;
use redis::{AsyncCommands, Client, aio::MultiplexedConnection};

pub struct Redis {
    pub client: Client,
}

impl Redis {
    pub async fn init(url:&str) -> Result<Self> {
        let client = Client::open(url)?;
        Ok(Self { client })
    }

    pub async fn get_conn(&self) -> Result<MultiplexedConnection> {
        let conn = self.client.get_multiplexed_async_connection().await?;
        Ok(conn)
    }

    pub async fn enqueue(&self, queue_name: &str, value: &str) -> Result<()> {
        let mut conn = self.get_conn().await?;
        let _: () = conn.lpush(queue_name, value).await?;
        Ok(())
    }

    pub async fn dequeue(&self, queue_name: &str) -> Result<Option<String>> {
        let mut conn = self.get_conn().await?;
        let result: Option<(String, String)> = conn.brpop(queue_name, 0f64).await?;
        Ok(result.map(|(_, val)| val))
    }
}