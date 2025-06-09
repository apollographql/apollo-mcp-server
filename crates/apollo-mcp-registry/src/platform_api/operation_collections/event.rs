use super::collection_poller::OperationData;
use super::error::CollectionError;

pub enum CollectionEvent {
    UpdateOperationCollection(Vec<OperationData>),
    CollectionError(CollectionError),
}
