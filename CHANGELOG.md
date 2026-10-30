# Changelog

## 0.2.0

Breaking: `Options` has new fields (use `..Options::default()`) and `Error` has a new `ReadOnly` variant.

- Reverse iteration (`retreat`, `seek_to_last`, `seek_for_prev`, `rev`, `last_key_value`)
- Prefix scans, `DbIter::seek`, `delete_range`, `delete_prefix`, `get_many`, `contains_key`
- `transact`, `compare_and_swap`, `increment`, `Transaction::range`
- LRU block cache for decompressed blocks (`Options::block_cache_bytes`, off by default)
- `Db::checkpoint`, `verify_integrity`, `table_info`, `approximate_size`
- Compaction filter, `TtlDb`, buckets
- Read-only open (`Options::read_only`, `Error::ReadOnly`)
- More counters in `Stats`; `Options::validate` runs on open
- `lsmcli` and `server` (TCP line protocol) examples
