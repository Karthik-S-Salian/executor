use anyhow::Result;
use deadpool_postgres::{Config, ManagerConfig, Pool, RecyclingMethod};
use tokio_postgres::{NoTls, Row, Statement, types::ToSql};
use url::Url;

fn init_pool(url: &str) -> Result<Pool> {
    let parsed = Url::parse(url).expect("Invalid URL");

    let mut cfg = Config::new();
    cfg.user = Some(parsed.username().to_string());
    cfg.password = parsed.password().map(str::to_string);
    cfg.host = parsed.host_str().map(str::to_string);
    cfg.port = parsed.port();
    cfg.dbname = parsed.path().strip_prefix('/').map(str::to_string);
    // notls -> sslmode disable

    cfg.manager = Some(ManagerConfig {
        recycling_method: RecyclingMethod::Fast,
    });
    let pool = cfg.create_pool(None, NoTls)?;

    Ok(pool)
}

pub struct Db {
    pub pool: Pool,
}

impl Db {
    pub async fn init(url: &str) -> Result<Self> {
        let pool = init_pool(url)?;
        let db = Db { pool };
        db.create_tables().await;
        Ok(db)
    }

    async fn create_tables(&self) {
        let client = self.pool.get().await.expect("Failed to get DB client");

        const SCHEMA_SQL: &str =
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../sql/up.sql"));

        if let Err(e) = client.batch_execute(SCHEMA_SQL).await {
            panic!("Failed to execute statement:\nError: {}", e);
        }
    }

    pub async fn query(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>> {
        let client = self.pool.get().await?;
        let stmt: Statement = client.prepare_cached(sql).await?;
        let rows = client.query(&stmt, params).await?;
        Ok(rows)
    }

    pub async fn query_one(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Row> {
        let client = self.pool.get().await?;
        let stmt: Statement = client.prepare_cached(sql).await?;
        let row = client.query_one(&stmt, params).await?;
        Ok(row)
    }

    pub async fn query_opt(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<Row>, anyhow::Error> {
        let client = self.pool.get().await?;
        let stmt: Statement = client.prepare_cached(sql).await?;
        let row = client.query_opt(&stmt, params).await?;
        Ok(row)
    }

    pub async fn execute(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64> {
        let client = self.pool.get().await?;
        let stmt: Statement = client.prepare(sql).await?;
        let rows_effected = client.execute(&stmt, params).await?;
        Ok(rows_effected)
    }
}
