use sea_orm_migration::MigratorTrait;

fn main() {
    let migrations = media_storage::Migrator::migrations();
    let latest = migrations
        .last()
        .expect("media-storage must register at least one migration");
    println!("{}", latest.name());
}
