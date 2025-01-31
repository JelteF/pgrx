/*
Portions Copyright 2019-2021 ZomboDB, LLC.
Portions Copyright 2021-2022 Technology Concepts & Design, Inc. <support@tcdi.com>

All rights reserved.

Use of this source code is governed by the MIT license that can be found in the LICENSE file.
*/
use pgx::prelude::*;
use pgx::IntoDatum;

pgx::pg_module_magic!();

extension_sql!(
    r#"

CREATE TABLE dog_daycare (
    dog_name varchar(256),
    dog_age int,
    dog_breed varchar(256)
);

INSERT INTO dog_daycare(dog_name, dog_age, dog_breed) VALUES ('Fido', 3, 'Labrador');
INSERT INTO dog_daycare(dog_name, dog_age, dog_breed) VALUES ('Spot', 5, 'Poodle');
INSERT INTO dog_daycare(dog_name, dog_age, dog_breed) VALUES ('Rover', 7, 'Golden Retriever');
INSERT INTO dog_daycare(dog_name, dog_age, dog_breed) VALUES ('Snoopy', 9, 'Beagle');
INSERT INTO dog_daycare(dog_name, dog_age, dog_breed) VALUES ('Lassie', 11, 'Collie');
INSERT INTO dog_daycare(dog_name, dog_age, dog_breed) VALUES ('Scooby', 13, 'Great Dane');
INSERT INTO dog_daycare(dog_name, dog_age, dog_breed) VALUES ('Moomba', 15, 'Labrador');


"#,
    name = "create_dog_daycare_example_table",
);

#[pg_extern]
fn calculate_human_years() -> Result<
    TableIterator<
        'static,
        (
            name!(dog_name, Result<Option<String>, pgx::spi::Error>),
            name!(dog_age, i32),
            name!(dog_breed, Result<Option<String>, pgx::spi::Error>),
            name!(human_age, i32),
        ),
    >,
    spi::Error,
> {
    /*
        This function is a simple example of using SPI to return a set of rows
        from a query. This query will return the same rows as the table, but
        with an additional column that is the dog's age in human years.
    */
    let query = "SELECT * FROM spi_srf.dog_daycare;";

    Spi::connect(|client| {
        let mut results = Vec::new();
        let mut tup_table = client.select(query, None, None)?;

        while let Some(row) = tup_table.next() {
            let dog_name = row["dog_name"].value::<String>();
            let dog_age = row["dog_age"].value::<i32>()?.expect("dog_age was null");
            let dog_breed = row["dog_breed"].value::<String>();
            let human_age = dog_age * 7;
            results.push((dog_name, dog_age, dog_breed, human_age));
        }

        Ok(TableIterator::new(results.into_iter()))
    })
}

#[pg_extern]
fn filter_by_breed(
    breed: &str,
) -> Result<
    TableIterator<
        'static,
        (
            name!(dog_name, Result<Option<String>, pgx::spi::Error>),
            name!(dog_age, Result<Option<i32>, pgx::spi::Error>),
            name!(dog_breed, Result<Option<String>, pgx::spi::Error>),
        ),
    >,
    spi::Error,
> {
    /*
        This function is a simple example of using SPI to return a set of rows
        from a query. This query will return the records for the given breed.
    */

    let query = "SELECT * FROM spi_srf.dog_daycare WHERE dog_breed = $1;";
    let args = vec![(PgBuiltInOids::TEXTOID.oid(), breed.into_datum())];

    Spi::connect(|client| {
        let tup_table = client.select(query, None, Some(args))?;

        let filtered = tup_table
            .map(|row| (row["dog_name"].value(), row["dog_age"].value(), row["dog_breed"].value()))
            .collect::<Vec<_>>();
        Ok(TableIterator::new(filtered.into_iter()))
    })
}

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use crate::calculate_human_years;
    use pgx::prelude::*;

    #[rustfmt::skip]
    #[pg_test]
    fn test_calculate_human_years() -> Result<(), pgx::spi::Error> {
        let mut results: Vec<(Result<Option<String>, _>, i32, Result<Option<String>, _>, i32)> =
            Vec::new();

        results.push((Ok(Some("Fido".to_string())), 3, Ok(Some("Labrador".to_string())), 21));
        results.push((Ok(Some("Spot".to_string())), 5, Ok(Some("Poodle".to_string())), 35));
        results.push((Ok(Some("Rover".to_string())), 7, Ok(Some("Golden Retriever".to_string())), 49));
        results.push((Ok(Some("Snoopy".to_string())), 9, Ok(Some("Beagle".to_string())), 63));
        results.push((Ok(Some("Lassie".to_string())), 11, Ok(Some("Collie".to_string())), 77));
        results.push((Ok(Some("Scooby".to_string())), 13, Ok(Some("Great Dane".to_string())), 91));
        results.push((Ok(Some("Moomba".to_string())), 15, Ok(Some("Labrador".to_string())), 105));
        let func_results = calculate_human_years()?;

        for (expected, actual) in results.iter().zip(func_results) {
            assert_eq!(expected, &actual);
        }
        Ok(())
    }
}

#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {
        // perform one-off initialization when the pg_test framework starts
    }

    pub fn postgresql_conf_options() -> Vec<&'static str> {
        // return any postgresql.conf settings that are required for your tests
        vec![]
    }
}
