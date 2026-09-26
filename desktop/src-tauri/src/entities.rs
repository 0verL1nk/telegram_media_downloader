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

pub mod legacy_retry {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "legacy_retry_ids")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub chat_id: String,
        pub message_id: i64,
        pub imported_at: String,
        pub claimed: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod upload_task {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "upload_tasks")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub source_task_id: Option<String>,
        pub source_path: String,
        pub remote_path: String,
        pub executable_path: String,
        pub zip_before_upload: bool,
        pub delete_local_after_upload: bool,
        pub status: String,
        pub uploaded_bytes: i64,
        pub total_bytes: i64,
        pub speed_bytes_per_second: i64,
        pub error: Option<String>,
        pub created_at: String,
        pub updated_at: String,
        pub completed_at: Option<String>,
        pub retry_count: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod telegram_transfer_task {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "telegram_transfer_tasks")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub kind: String,
        pub source_task_id: Option<String>,
        pub source_chat_id: Option<String>,
        pub source_message_id: Option<i64>,
        pub target_chat_id: String,
        pub delete_local_after_upload: bool,
        pub status: String,
        pub total_bytes: Option<i64>,
        pub sent_bytes: Option<i64>,
        pub result_message_id: Option<i64>,
        pub error: Option<String>,
        pub created_at: String,
        pub updated_at: String,
        pub started_at: Option<String>,
        pub completed_at: Option<String>,
        pub retry_count: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
