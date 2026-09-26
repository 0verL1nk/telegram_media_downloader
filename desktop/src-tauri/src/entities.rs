pub mod task {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "tasks")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub chat_id: String,
        pub message_id: i64,
        pub title: String,
        pub media_type: String,
        pub file_name: String,
        pub target_path: String,
        pub total_bytes: i64,
        pub completed_bytes: i64,
        pub speed_bytes_per_second: i64,
        pub status: String,
        pub error: Option<String>,
        pub created_at: String,
        pub updated_at: String,
        pub started_at: Option<String>,
        pub completed_at: Option<String>,
        pub group_id: Option<String>,
        pub retry_count: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod chunk {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "chunks")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub task_id: String,
        pub offset_bytes: i64,
        pub length_bytes: i64,
        pub digest: Option<String>,
        pub complete: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
