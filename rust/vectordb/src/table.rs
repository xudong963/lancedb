// Copyright 2023 Lance Developers.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::path::Path;
use std::sync::Arc;

use arrow_array::{Float32Array, RecordBatchReader};
use lance::dataset::{Dataset, WriteMode, WriteParams};
use lance::index::IndexType;

use crate::error::{Error, Result};
use crate::index::vector::VectorIndexBuilder;
use crate::query::Query;

pub const VECTOR_COLUMN_NAME: &str = "vector";
pub const LANCE_FILE_EXTENSION: &str = "lance";

/// A table in a LanceDB database.
pub struct Table {
    name: String,
    uri: String,
    dataset: Arc<Dataset>,
}

impl std::fmt::Display for Table {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Table({})", self.name)
    }
}

impl Table {
    /// Opens an existing Table
    ///
    /// # Arguments
    ///
    /// * `base_path` - The base path where the table is located
    /// * `name` The Table name
    ///
    /// # Returns
    ///
    /// * A [Table] object.
    pub async fn open(base_uri: &str, name: &str) -> Result<Self> {
        let path = Path::new(base_uri);

        let table_uri = path.join(format!("{}.{}", name, LANCE_FILE_EXTENSION));
        let uri = table_uri
            .as_path()
            .to_str()
            .ok_or(Error::IO(format!("Invalid table name: {}", name)))?;

        let dataset = Dataset::open(&uri).await?;
        Ok(Table {
            name: name.to_string(),
            uri: uri.to_string(),
            dataset: Arc::new(dataset),
        })
    }

    /// Creates a new Table
    ///
    /// # Arguments
    ///
    /// * `base_path` - The base path where the table is located
    /// * `name` The Table name
    /// * `batches` RecordBatch to be saved in the database
    ///
    /// # Returns
    ///
    /// * A [Table] object.
    pub async fn create(
        base_uri: &str,
        name: &str,
        mut batches: Box<dyn RecordBatchReader>,
    ) -> Result<Self> {
        let base_path = Path::new(base_uri);
        let table_uri = base_path.join(format!("{}.{}", name, LANCE_FILE_EXTENSION));
        let uri = table_uri
            .as_path()
            .to_str()
            .ok_or(Error::IO(format!("Invalid table name: {}", name)))?
            .to_string();
        let dataset =
            Arc::new(Dataset::write(&mut batches, &uri, Some(WriteParams::default())).await?);
        Ok(Table {
            name: name.to_string(),
            uri,
            dataset,
        })
    }

    /// Create index on the table.
    pub async fn create_index(&mut self, index_builder: &impl VectorIndexBuilder) -> Result<()> {
        use lance::index::DatasetIndexExt;

        let dataset = self
            .dataset
            .create_index(
                &[index_builder
                    .get_column()
                    .unwrap_or(VECTOR_COLUMN_NAME.to_string())
                    .as_str()],
                IndexType::Vector,
                index_builder.get_index_name(),
                &index_builder.build(),
            )
            .await?;
        self.dataset = Arc::new(dataset);
        Ok(())
    }

    /// Insert records into this Table
    ///
    /// # Arguments
    ///
    /// * `batches` RecordBatch to be saved in the Table
    /// * `write_mode` Append / Overwrite existing records. Default: Append
    /// # Returns
    ///
    /// * The number of rows added
    pub async fn add(
        &mut self,
        mut batches: Box<dyn RecordBatchReader>,
        write_mode: Option<WriteMode>,
    ) -> Result<usize> {
        let mut params = WriteParams::default();
        params.mode = write_mode.unwrap_or(WriteMode::Append);

        self.dataset = Arc::new(Dataset::write(&mut batches, &self.uri, Some(params)).await?);
        Ok(batches.count())
    }

    /// Creates a new Query object that can be executed.
    ///
    /// # Arguments
    ///
    /// * `vector` The vector used for this query.
    ///
    /// # Returns
    ///
    /// * A [Query] object.
    pub fn search(&self, query_vector: Float32Array) -> Query {
        Query::new(self.dataset.clone(), query_vector)
    }

    /// Returns the number of rows in this Table
    pub async fn count_rows(&self) -> Result<usize> {
        Ok(self.dataset.count_rows().await?)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{
        Array, FixedSizeListArray, Float32Array, Int32Array, RecordBatch, RecordBatchReader,
    };
    use arrow_data::ArrayDataBuilder;
    use arrow_schema::{DataType, Field, Schema};
    use lance::arrow::RecordBatchBuffer;
    use lance::dataset::{Dataset, WriteMode};
    use lance::index::vector::ivf::IvfBuildParams;
    use lance::index::vector::pq::PQBuildParams;
    use rand::Rng;
    use tempfile::tempdir;

    use super::*;
    use crate::index::vector::IvfPQIndexBuilder;

    #[tokio::test]
    async fn test_new_table_not_exists() {
        let tmp_dir = tempdir().unwrap();
        let uri = tmp_dir.path().to_str().unwrap();

        let table = Table::open(&uri, "test").await;
        assert!(table.is_err());
    }

    #[tokio::test]
    async fn test_open() {
        let tmp_dir = tempdir().unwrap();
        let dataset_path = tmp_dir.path().join("test.lance");
        let uri = tmp_dir.path().to_str().unwrap();

        let mut batches: Box<dyn RecordBatchReader> = Box::new(make_test_batches());
        Dataset::write(&mut batches, dataset_path.to_str().unwrap(), None)
            .await
            .unwrap();

        let table = Table::open(uri, "test").await.unwrap();

        assert_eq!(table.name, "test")
    }

    #[test]
    fn test_object_store_path() {
        use std::path::Path as StdPath;
        let p = StdPath::new("s3://bucket/path/to/file");
        let c = p.join("subfile");
        assert_eq!(c.to_str().unwrap(), "s3://bucket/path/to/file/subfile");
    }

    #[tokio::test]
    async fn test_add() {
        let tmp_dir = tempdir().unwrap();
        let uri = tmp_dir.path().to_str().unwrap();

        let batches: Box<dyn RecordBatchReader> = Box::new(make_test_batches());
        let schema = batches.schema().clone();
        let mut table = Table::create(&uri, "test", batches).await.unwrap();
        assert_eq!(table.count_rows().await.unwrap(), 10);

        let new_batches: Box<dyn RecordBatchReader> =
            Box::new(RecordBatchBuffer::new(vec![RecordBatch::try_new(
                schema,
                vec![Arc::new(Int32Array::from_iter_values(100..110))],
            )
            .unwrap()]));

        table.add(new_batches, None).await.unwrap();
        assert_eq!(table.count_rows().await.unwrap(), 20);
        assert_eq!(table.name, "test");
    }

    #[tokio::test]
    async fn test_add_overwrite() {
        let tmp_dir = tempdir().unwrap();
        let uri = tmp_dir.path().to_str().unwrap();

        let batches: Box<dyn RecordBatchReader> = Box::new(make_test_batches());
        let schema = batches.schema().clone();
        let mut table = Table::create(uri, "test", batches).await.unwrap();
        assert_eq!(table.count_rows().await.unwrap(), 10);

        let new_batches: Box<dyn RecordBatchReader> =
            Box::new(RecordBatchBuffer::new(vec![RecordBatch::try_new(
                schema,
                vec![Arc::new(Int32Array::from_iter_values(100..110))],
            )
            .unwrap()]));

        table
            .add(new_batches, Some(WriteMode::Overwrite))
            .await
            .unwrap();
        assert_eq!(table.count_rows().await.unwrap(), 10);
        assert_eq!(table.name, "test");
    }

    #[tokio::test]
    async fn test_search() {
        let tmp_dir = tempdir().unwrap();
        let dataset_path = tmp_dir.path().join("test.lance");
        let uri = tmp_dir.path().to_str().unwrap();

        let mut batches: Box<dyn RecordBatchReader> = Box::new(make_test_batches());
        Dataset::write(&mut batches, dataset_path.to_str().unwrap(), None)
            .await
            .unwrap();

        let table = Table::open(uri, "test").await.unwrap();

        let vector = Float32Array::from_iter_values([0.1, 0.2]);
        let query = table.search(vector.clone());
        assert_eq!(vector, query.query_vector);
    }

    fn make_test_batches() -> RecordBatchBuffer {
        let schema = Arc::new(Schema::new(vec![Field::new("i", DataType::Int32, false)]));
        RecordBatchBuffer::new(vec![RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(Int32Array::from_iter_values(0..10))],
        )
        .unwrap()])
    }

    #[tokio::test]
    async fn test_create_index() {
        use arrow_array::RecordBatch;
        use arrow_schema::{DataType, Field, Schema as ArrowSchema};
        use rand;
        use std::iter::repeat_with;

        use arrow_array::Float32Array;

        let tmp_dir = tempdir().unwrap();
        let uri = tmp_dir.path().to_str().unwrap();

        let dimension = 16;
        let schema = Arc::new(ArrowSchema::new(vec![Field::new(
            "embeddings",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dimension,
            ),
            false,
        )]));

        let mut rng = rand::thread_rng();
        let float_arr = Float32Array::from(
            repeat_with(|| rng.gen::<f32>())
                .take(512 * dimension as usize)
                .collect::<Vec<f32>>(),
        );

        let vectors = Arc::new(create_fixed_size_list(float_arr, dimension).unwrap());
        let batches = RecordBatchBuffer::new(vec![RecordBatch::try_new(
            schema.clone(),
            vec![vectors.clone()],
        )
        .unwrap()]);

        let reader: Box<dyn RecordBatchReader + Send> = Box::new(batches);
        let mut table = Table::create(uri, "test", reader).await.unwrap();

        let mut i = IvfPQIndexBuilder::new();

        let index_builder = i
            .column("embeddings".to_string())
            .index_name("my_index".to_string())
            .ivf_params(IvfBuildParams::new(256))
            .pq_params(PQBuildParams::default());

        table.create_index(index_builder).await.unwrap();

        assert_eq!(table.dataset.load_indices().await.unwrap().len(), 1);
        assert_eq!(table.count_rows().await.unwrap(), 512);
        assert_eq!(table.name, "test");
    }

    fn create_fixed_size_list<T: Array>(values: T, list_size: i32) -> Result<FixedSizeListArray> {
        let list_type = DataType::FixedSizeList(
            Arc::new(Field::new("item", values.data_type().clone(), true)),
            list_size,
        );
        let data = ArrayDataBuilder::new(list_type)
            .len(values.len() / list_size as usize)
            .add_child_data(values.into_data())
            .build()
            .unwrap();

        Ok(FixedSizeListArray::from(data))
    }
}
