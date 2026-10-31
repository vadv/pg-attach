use crate::postgres::PostgresPair;
use cucumber::World;

#[derive(Debug, Default, World)]
pub struct AttachWorld {
    pub pair: Option<PostgresPair>,
}

impl AttachWorld {
    pub fn expand_sql(&self, sql: &str) -> String {
        let pair = self.pair.as_ref().expect("PostgreSQL pair is not started");
        sql.replace("${SOURCE_PORT}", &pair.source_port().to_string())
            .replace("${TARGET_PORT}", &pair.target_port().to_string())
    }

    pub async fn execute(&self, side: &str, sql: &str) -> Result<(), String> {
        let sql = self.expand_sql(sql);
        self.pair
            .as_ref()
            .expect("PostgreSQL pair is not started")
            .execute(side, &sql)
            .await
    }

    pub async fn query(&self, side: &str, sql: &str) -> Result<String, String> {
        let sql = self.expand_sql(sql);
        self.pair
            .as_ref()
            .expect("PostgreSQL pair is not started")
            .query(side, &sql)
            .await
    }
}
