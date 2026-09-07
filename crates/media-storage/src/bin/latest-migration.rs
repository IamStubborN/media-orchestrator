use sea_orm_migration::MigratorTrait;

fn main() {
    let migrations = media_storage::Migrator::migrations();
    let names: Vec<String> = migrations
        .iter()
        .map(|migration| migration.name().to_owned())
        .collect();
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        None => println!(
            "{}",
            names
                .last()
                .expect("media-storage must register at least one migration")
        ),
        Some("--with-predecessor") => {
            assert!(
                arguments.next().is_none(),
                "unexpected latest-migration argument"
            );
            let latest = names
                .last()
                .expect("media-storage must register at least one migration");
            let predecessor = names
                .len()
                .checked_sub(2)
                .map(|index| names[index].as_str())
                .expect("first migration has no predecessor");
            println!("{latest}");
            println!("{predecessor}");
        }
        Some("--predecessor-of") => {
            let target = arguments
                .next()
                .expect("--predecessor-of requires a migration name");
            assert!(
                arguments.next().is_none(),
                "unexpected latest-migration argument"
            );
            let position = names
                .iter()
                .position(|name| name == &target)
                .unwrap_or_else(|| panic!("migration is not registered: {target}"));
            let predecessor = position
                .checked_sub(1)
                .expect("first migration has no predecessor");
            println!("{}", names[predecessor]);
        }
        Some(argument) => panic!("unsupported latest-migration argument: {argument}"),
    }
}
