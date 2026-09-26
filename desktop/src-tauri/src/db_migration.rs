use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260926_000001_tasks::Migration),
            Box::new(m20260926_000002_upload_tasks::Migration),
            Box::new(m20260926_000003_telegram_transfers::Migration),
            Box::new(m20260926_000004_tasks_media_url::Migration),
        ]
    }
}

mod m20260926_000004_tasks_media_url {
    use sea_orm_migration::prelude::*;

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m20260926_000004_tasks_media_url"
        }
    }

    #[derive(DeriveIden)]
    enum Tasks {
        Table,
        MediaUrl,
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .alter_table(
                    Table::alter()
                        .table(Tasks::Table)
                        .add_column(ColumnDef::new(Tasks::MediaUrl).string().null())
                        .to_owned(),
                )
                .await
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .alter_table(
                    Table::alter()
                        .table(Tasks::Table)
                        .drop_column(Tasks::MediaUrl)
                        .to_owned(),
                )
                .await
        }
    }
}

mod m20260926_000003_telegram_transfers {
    use sea_orm_migration::prelude::*;

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m20260926_000003_telegram_transfers"
        }
    }

    #[derive(DeriveIden)]
    enum TelegramTransferTasks {
        Table,
        Id,
        Kind,
        SourceTaskId,
        SourceChatId,
        SourceMessageId,
        TargetChatId,
        DeleteLocalAfterUpload,
        Status,
        TotalBytes,
        SentBytes,
        ResultMessageId,
        Error,
        CreatedAt,
        UpdatedAt,
        StartedAt,
        CompletedAt,
        RetryCount,
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .create_table(
                    Table::create()
                        .table(TelegramTransferTasks::Table)
                        .if_not_exists()
                        .col(
                            ColumnDef::new(TelegramTransferTasks::Id)
                                .string()
                                .not_null()
                                .primary_key(),
                        )
                        .col(
                            ColumnDef::new(TelegramTransferTasks::Kind)
                                .string()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(TelegramTransferTasks::SourceTaskId)
                                .string()
                                .null(),
                        )
                        .col(
                            ColumnDef::new(TelegramTransferTasks::SourceChatId)
                                .string()
                                .null(),
                        )
                        .col(
                            ColumnDef::new(TelegramTransferTasks::SourceMessageId)
                                .big_integer()
                                .null(),
                        )
                        .col(
                            ColumnDef::new(TelegramTransferTasks::TargetChatId)
                                .string()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(TelegramTransferTasks::DeleteLocalAfterUpload)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .col(
                            ColumnDef::new(TelegramTransferTasks::Status)
                                .string()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(TelegramTransferTasks::TotalBytes)
                                .big_integer()
                                .null(),
                        )
                        .col(
                            ColumnDef::new(TelegramTransferTasks::SentBytes)
                                .big_integer()
                                .null(),
                        )
                        .col(
                            ColumnDef::new(TelegramTransferTasks::ResultMessageId)
                                .big_integer()
                                .null(),
                        )
                        .col(ColumnDef::new(TelegramTransferTasks::Error).string().null())
                        .col(
                            ColumnDef::new(TelegramTransferTasks::CreatedAt)
                                .string()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(TelegramTransferTasks::UpdatedAt)
                                .string()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(TelegramTransferTasks::StartedAt)
                                .string()
                                .null(),
                        )
                        .col(
                            ColumnDef::new(TelegramTransferTasks::CompletedAt)
                                .string()
                                .null(),
                        )
                        .col(
                            ColumnDef::new(TelegramTransferTasks::RetryCount)
                                .integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_telegram_transfer_status_created")
                        .table(TelegramTransferTasks::Table)
                        .col(TelegramTransferTasks::Status)
                        .col(TelegramTransferTasks::CreatedAt)
                        .to_owned(),
                )
                .await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(TelegramTransferTasks::Table).to_owned())
                .await
        }
    }
}

mod m20260926_000002_upload_tasks {
    use sea_orm_migration::prelude::*;

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m20260926_000002_upload_tasks"
        }
    }

    #[derive(DeriveIden)]
    enum UploadTasks {
        Table,
        Id,
        SourceTaskId,
        SourcePath,
        RemotePath,
        ExecutablePath,
        ZipBeforeUpload,
        DeleteLocalAfterUpload,
        Status,
        UploadedBytes,
        TotalBytes,
        SpeedBytesPerSecond,
        Error,
        CreatedAt,
        UpdatedAt,
        CompletedAt,
        RetryCount,
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .create_table(
                    Table::create()
                        .table(UploadTasks::Table)
                        .if_not_exists()
                        .col(
                            ColumnDef::new(UploadTasks::Id)
                                .string()
                                .not_null()
                                .primary_key(),
                        )
                        .col(ColumnDef::new(UploadTasks::SourceTaskId).string().null())
                        .col(ColumnDef::new(UploadTasks::SourcePath).string().not_null())
                        .col(ColumnDef::new(UploadTasks::RemotePath).string().not_null())
                        .col(
                            ColumnDef::new(UploadTasks::ExecutablePath)
                                .string()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(UploadTasks::ZipBeforeUpload)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .col(
                            ColumnDef::new(UploadTasks::DeleteLocalAfterUpload)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .col(ColumnDef::new(UploadTasks::Status).string().not_null())
                        .col(
                            ColumnDef::new(UploadTasks::UploadedBytes)
                                .big_integer()
                                .not_null()
                                .default(0),
                        )
                        .col(
                            ColumnDef::new(UploadTasks::TotalBytes)
                                .big_integer()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(UploadTasks::SpeedBytesPerSecond)
                                .big_integer()
                                .not_null()
                                .default(0),
                        )
                        .col(ColumnDef::new(UploadTasks::Error).string().null())
                        .col(ColumnDef::new(UploadTasks::CreatedAt).string().not_null())
                        .col(ColumnDef::new(UploadTasks::UpdatedAt).string().not_null())
                        .col(ColumnDef::new(UploadTasks::CompletedAt).string().null())
                        .col(
                            ColumnDef::new(UploadTasks::RetryCount)
                                .integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_upload_tasks_status_created")
                        .table(UploadTasks::Table)
                        .col(UploadTasks::Status)
                        .col(UploadTasks::CreatedAt)
                        .to_owned(),
                )
                .await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(UploadTasks::Table).to_owned())
                .await
        }
    }
}

mod m20260926_000001_tasks {
    use sea_orm_migration::prelude::*;

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m20260926_000001_tasks"
        }
    }

    #[derive(DeriveIden)]
    enum Tasks {
        Table,
        Id,
        ChatId,
        MessageId,
        Title,
        MediaType,
        FileName,
        TargetPath,
        TotalBytes,
        CompletedBytes,
        SpeedBytesPerSecond,
        Status,
        Error,
        CreatedAt,
        UpdatedAt,
        StartedAt,
        CompletedAt,
        GroupId,
        RetryCount,
    }
    #[derive(DeriveIden)]
    enum Chunks {
        Table,
        Id,
        TaskId,
        OffsetBytes,
        LengthBytes,
        Digest,
        Complete,
    }
    #[derive(DeriveIden)]
    enum LegacyRetryIds {
        Table,
        Id,
        ChatId,
        MessageId,
        ImportedAt,
        Claimed,
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .create_table(
                    Table::create()
                        .table(Tasks::Table)
                        .if_not_exists()
                        .col(ColumnDef::new(Tasks::Id).string().not_null().primary_key())
                        .col(ColumnDef::new(Tasks::ChatId).string().not_null())
                        .col(ColumnDef::new(Tasks::MessageId).big_integer().not_null())
                        .col(ColumnDef::new(Tasks::Title).string().not_null().default(""))
                        .col(
                            ColumnDef::new(Tasks::MediaType)
                                .string()
                                .not_null()
                                .default(""),
                        )
                        .col(
                            ColumnDef::new(Tasks::FileName)
                                .string()
                                .not_null()
                                .default(""),
                        )
                        .col(ColumnDef::new(Tasks::TargetPath).string().not_null())
                        .col(
                            ColumnDef::new(Tasks::TotalBytes)
                                .big_integer()
                                .not_null()
                                .default(0),
                        )
                        .col(
                            ColumnDef::new(Tasks::CompletedBytes)
                                .big_integer()
                                .not_null()
                                .default(0),
                        )
                        .col(
                            ColumnDef::new(Tasks::SpeedBytesPerSecond)
                                .big_integer()
                                .not_null()
                                .default(0),
                        )
                        .col(ColumnDef::new(Tasks::Status).string().not_null())
                        .col(ColumnDef::new(Tasks::Error).string().null())
                        .col(ColumnDef::new(Tasks::CreatedAt).string().not_null())
                        .col(ColumnDef::new(Tasks::UpdatedAt).string().not_null())
                        .col(ColumnDef::new(Tasks::StartedAt).string().null())
                        .col(ColumnDef::new(Tasks::CompletedAt).string().null())
                        .col(ColumnDef::new(Tasks::GroupId).string().null())
                        .col(
                            ColumnDef::new(Tasks::RetryCount)
                                .integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_tasks_status_created")
                        .table(Tasks::Table)
                        .col(Tasks::Status)
                        .col(Tasks::CreatedAt)
                        .to_owned(),
                )
                .await?;
            manager
                .create_table(
                    Table::create()
                        .table(Chunks::Table)
                        .if_not_exists()
                        .col(
                            ColumnDef::new(Chunks::Id)
                                .big_integer()
                                .not_null()
                                .auto_increment()
                                .primary_key(),
                        )
                        .col(ColumnDef::new(Chunks::TaskId).string().not_null())
                        .col(ColumnDef::new(Chunks::OffsetBytes).big_integer().not_null())
                        .col(ColumnDef::new(Chunks::LengthBytes).big_integer().not_null())
                        .col(ColumnDef::new(Chunks::Digest).string().null())
                        .col(
                            ColumnDef::new(Chunks::Complete)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .foreign_key(
                            ForeignKey::create()
                                .name("fk_chunks_task")
                                .from(Chunks::Table, Chunks::TaskId)
                                .to(Tasks::Table, Tasks::Id)
                                .on_delete(ForeignKeyAction::Cascade),
                        )
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("uq_chunks_task_offset")
                        .table(Chunks::Table)
                        .col(Chunks::TaskId)
                        .col(Chunks::OffsetBytes)
                        .unique()
                        .to_owned(),
                )
                .await?;
            manager
                .create_table(
                    Table::create()
                        .table(LegacyRetryIds::Table)
                        .if_not_exists()
                        .col(
                            ColumnDef::new(LegacyRetryIds::Id)
                                .string()
                                .not_null()
                                .primary_key(),
                        )
                        .col(ColumnDef::new(LegacyRetryIds::ChatId).string().not_null())
                        .col(
                            ColumnDef::new(LegacyRetryIds::MessageId)
                                .big_integer()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(LegacyRetryIds::ImportedAt)
                                .string()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(LegacyRetryIds::Claimed)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(LegacyRetryIds::Table).to_owned())
                .await?;
            manager
                .drop_table(Table::drop().table(Chunks::Table).to_owned())
                .await?;
            manager
                .drop_table(Table::drop().table(Tasks::Table).to_owned())
                .await
        }
    }
}
