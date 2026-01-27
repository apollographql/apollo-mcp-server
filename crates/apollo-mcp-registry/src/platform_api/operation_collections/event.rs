use super::collection_poller::OperationData;
use super::error::CollectionError;

#[derive(Debug)]
pub enum CollectionEvent {
    UpdateOperationCollection(Vec<OperationData>),
    CollectionError(CollectionError),
}
