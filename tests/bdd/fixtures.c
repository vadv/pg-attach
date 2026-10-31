/* Test-only page surgery; this library is never installed in the extension image. */
#include "postgres.h"
#include "fmgr.h"
#include "miscadmin.h"
#include "access/heapam.h"
#include "access/htup_details.h"
#include "access/table.h"
#include "access/xloginsert.h"
#include "storage/bufmgr.h"

PG_MODULE_MAGIC;
PG_FUNCTION_INFO_V1(pg_attach_test_legacy_xmin);

Datum
pg_attach_test_legacy_xmin(PG_FUNCTION_ARGS)
{
    Relation rel;
    BlockNumber blocks;
    Buffer buffer;
    Page page;
    ItemId item;
    HeapTupleHeader header;

    if (!superuser())
        elog(ERROR, "legacy fixture requires superuser");
    rel = table_open(PG_GETARG_OID(0), AccessExclusiveLock);
    blocks = RelationGetNumberOfBlocks(rel);
    if (blocks == 0)
        elog(ERROR, "legacy fixture requires a nonempty heap");
    buffer = ReadBuffer(rel, blocks - 1);
    LockBuffer(buffer, BUFFER_LOCK_EXCLUSIVE);
    page = BufferGetPage(buffer);
    if (PageGetMaxOffsetNumber(page) < FirstOffsetNumber)
        elog(ERROR, "legacy fixture requires a tuple on the last block");
    item = PageGetItemId(page, FirstOffsetNumber);
    if (!ItemIdIsNormal(item))
        elog(ERROR, "legacy fixture requires a normal tuple");
    header = (HeapTupleHeader) PageGetItem(page, item);
    if (!HeapTupleHeaderXminFrozen(header) ||
        !(header->t_infomask & HEAP_XMAX_INVALID))
        elog(ERROR, "legacy fixture requires VACUUM FREEZE first");

    /* Pre-9.4 representation: raw xmin=2, without modern frozen hint bits.
     * Preserve VM intentionally: PostgreSQL considers this representation frozen. */
    START_CRIT_SECTION();
    HeapTupleHeaderSetXmin(header, FrozenTransactionId);
    header->t_infomask &= ~HEAP_XMIN_FROZEN;
    header->t_infomask |= HEAP_XMIN_COMMITTED;
    MarkBufferDirty(buffer);
    log_newpage_buffer(buffer, true);
    END_CRIT_SECTION();

    UnlockReleaseBuffer(buffer);
    table_close(rel, NoLock);
    PG_RETURN_VOID();
}
