use super::collection_poller::OperationData;
use super::error::CollectionError;

pub enum CollectionEvent {
    OperationCollectionUpdate(Vec<OperationData>),
    CollectionError(CollectionError),
}
