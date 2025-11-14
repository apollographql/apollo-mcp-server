### Index fields from interface implementing types - @DaleSeo PR #494

Fixed an issue where the search tool would not return results for fields that only exist on types implementing an interface. 
Now when a query returns an interface type, the search tool correctly indexes and searches all fields from implementing types, making implementation-specific fields discoverable even when accessed through interface types.