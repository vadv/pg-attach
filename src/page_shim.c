#include "postgres.h"

#include "access/detoast.h"
#include "access/heapam.h"
#include "access/heaptoast.h"
#include "access/htup_details.h"
#include "access/nbtree.h"
#include "access/toast_compression.h"
#include "access/xlog.h"
#include "access/xloginsert.h"
#include "access/xlogrecord.h"
#include "catalog/pg_am_d.h"
#include "miscadmin.h"
#include "catalog/namespace.h"
#include "storage/bufmgr.h"
#include "storage/bufpage.h"
#include "storage/checksum.h"
#include "storage/smgr.h"
#include "utils/rel.h"
#include "utils/relcache.h"
#include "utils/ruleutils.h"
#include "utils/regproc.h"

typedef struct PgAttachAttribute
{
    char        name[NAMEDATALEN];
    int32       typmod;
    int16       len;
    uint8       align;
    bool        by_val;
    uint8       storage;
    bool        has_missing;
    uint8       generated;
    bool        not_null;
    bool        dropped;
} PgAttachAttribute;

typedef struct PgAttachBtreeMetadata
{
    uint32      version;
    bool        all_equal_image;
    bool        expected_all_equal_image;
} PgAttachBtreeMetadata;

bool pg_attach_verify_page(Page page, BlockNumber blkno, bool checksums);

Oid
pg_attach_resolve_relation(const char *name)
{
    return RangeVarGetRelid(makeRangeVarFromNameList(stringToQualifiedNameList(name, NULL)),
                            NoLock, true);
}

void
pg_attach_flush_relation(Relation rel)
{
    FlushRelationBuffers(rel);
}

/* Streaming reads require table lock/checkpoint; validation uses lock/relation flush. */
void
pg_attach_read_page(Relation rel, ForkNumber fork, BlockNumber blkno, char *dst)
{
    SMgrRelation smgr = RelationGetSmgr(rel);

    if (!smgrexists(smgr, fork))
        elog(ERROR, "pg_attach: relation fork %d does not exist", fork);
    smgrread(smgr, fork, blkno, dst);
    if (fork == MAIN_FORKNUM &&
        !pg_attach_verify_page(dst, blkno, DataChecksumsEnabled()))
        ereport(ERROR,
                (errcode(ERRCODE_DATA_CORRUPTED),
                 errmsg("pg_attach: source page verification failed (checksum/layout) for block %u", blkno)));
}

bool
pg_attach_heap_page_is_frozen(Page page)
{
    OffsetNumber offnum;
    OffsetNumber maxoff = PageGetMaxOffsetNumber(page);

    for (offnum = FirstOffsetNumber; offnum <= maxoff; offnum++)
    {
        ItemId itemid;
        HeapTupleHeader header;

        CHECK_FOR_INTERRUPTS();
        itemid = PageGetItemId(page, offnum);
        if (!ItemIdIsNormal(itemid))
            continue;
        header = (HeapTupleHeader) PageGetItem(page, itemid);
        if (!HeapTupleHeaderXminFrozen(header) ||
            !(header->t_infomask & HEAP_XMAX_INVALID))
            return false;
    }
    return true;
}

static void
pg_attach_finish_page(Page page, BlockNumber blkno, bool checksums)
{
    PageSetLSN(page, 1);
    ((PageHeader) page)->pd_prune_xid = 0;
    if (checksums)
#if PG_VERSION_NUM >= 190000
        PageSetChecksum(page, blkno);
#else
        PageSetChecksumInplace(page, blkno);
#endif
    else
        ((PageHeader) page)->pd_checksum = 0;
}

void
pg_attach_prepare_page(Page page, BlockNumber blkno, bool checksums)
{
    pg_attach_finish_page(page, blkno, checksums);
}

bool
pg_attach_verify_page(Page page, BlockNumber blkno, bool checksums)
{
    PageHeader phdr = (PageHeader) page;
    uint16 stored;
    bool ok;
    int i;

    if (PageIsNew(page))
    {
        /* A new page is legitimate only when entirely zeroed; the checksum
         * setters skip such pages, so they must never pass by default. */
        for (i = 0; i < BLCKSZ; i++)
            if (((const char *) page)[i] != 0)
                return false;
        return true;
    }

    if (PageGetPageLayoutVersion(page) != PG_PAGE_LAYOUT_VERSION ||
        PageGetPageSize(page) != BLCKSZ ||
        phdr->pd_lower < SizeOfPageHeaderData ||
        phdr->pd_lower > phdr->pd_upper ||
        phdr->pd_upper > phdr->pd_special ||
        phdr->pd_special > BLCKSZ ||
        phdr->pd_special != MAXALIGN(phdr->pd_special))
        return false;

    if (!checksums)
        return true;

    stored = phdr->pd_checksum;
#if PG_VERSION_NUM >= 190000
    PageSetChecksum(page, blkno);
#else
    PageSetChecksumInplace(page, blkno);
#endif
    ok = phdr->pd_checksum == stored;
    phdr->pd_checksum = stored;
    return ok;
}

bool
pg_attach_verify_btree_metapage(Page page, bool all_equal_image)
{
    BTMetaPageData *metadata;

    if (PageIsNew(page))
        return false;
    metadata = BTPageGetMeta(page);
    return metadata->btm_magic == BTREE_MAGIC &&
        metadata->btm_version >= BTREE_MIN_VERSION &&
        metadata->btm_version <= BTREE_VERSION &&
        metadata->btm_allequalimage == all_equal_image;
}

void
pg_attach_btree_metadata(Relation rel, PgAttachBtreeMetadata *metadata)
{
    Buffer buffer;
    Page page;
    BTMetaPageData *metapage;

    buffer = ReadBuffer(rel, BTREE_METAPAGE);
    LockBuffer(buffer, BUFFER_LOCK_SHARE);
    page = BufferGetPage(buffer);
    if (PageIsNew(page))
        elog(ERROR, "pg_attach: btree metapage is uninitialized");
    metapage = BTPageGetMeta(page);
    if (metapage->btm_magic != BTREE_MAGIC ||
        metapage->btm_version < BTREE_MIN_VERSION ||
        metapage->btm_version > BTREE_VERSION)
        elog(ERROR, "pg_attach: unsupported btree metapage format");
    metadata->version = metapage->btm_version;
    metadata->all_equal_image = metapage->btm_allequalimage;
    metadata->expected_all_equal_image = _bt_allequalimage(rel, false);
    UnlockReleaseBuffer(buffer);
}

int
pg_attach_max_toast_chunk_size(void)
{
    return TOAST_MAX_CHUNK_SIZE;
}

int
pg_attach_max_identifier_length(void)
{
    return NAMEDATALEN - 1;
}

int
pg_attach_checksum_algorithm_version(void)
{
    return PG_DATA_CHECKSUM_VERSION;
}

bool
pg_attach_lz4_supported(void)
{
#ifdef USE_LZ4
    return true;
#else
    return false;
#endif
}

void
pg_attach_prepare_heap_page(Page page, Relation rel, BlockNumber blkno,
                            Oid source_toast_oid, Oid target_toast_oid,
                            bool patch_toast, bool target_lz4_supported,
                            bool checksums)
{
    TupleDesc desc = RelationGetDescr(rel);
    Datum *values = NULL;
    bool *isnull = NULL;
    OffsetNumber offnum;
    OffsetNumber maxoff = PageGetMaxOffsetNumber(page);

    ((PageHeader) page)->pd_prune_xid = 0;

    for (offnum = FirstOffsetNumber; offnum <= maxoff; offnum++)
    {
        ItemId itemid;
        HeapTupleHeader header;
        HeapTupleData tuple;
        int attno;
        int tuple_natts;

        CHECK_FOR_INTERRUPTS();
        itemid = PageGetItemId(page, offnum);
        if (!ItemIdIsNormal(itemid))
            continue;

        header = (HeapTupleHeader) PageGetItem(page, itemid);
        if (!HeapTupleHeaderXminFrozen(header) ||
            !(header->t_infomask & HEAP_XMAX_INVALID))
            ereport(ERROR,
                    (errcode(ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE),
                     errmsg("pg_attach: block %u contains a tuple that is not frozen; rerun VACUUM (FREEZE)", blkno)));

        tuple_natts = HeapTupleHeaderGetNatts(header);
        if (tuple_natts > desc->natts)
            ereport(ERROR,
                    (errcode(ERRCODE_DATA_CORRUPTED),
                     errmsg("pg_attach: tuple attribute count exceeds target descriptor")));
        for (attno = tuple_natts; attno < desc->natts; attno++)
            if (TupleDescAttr(desc, attno)->atthasmissing)
                ereport(ERROR,
                        (errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
                         errmsg("pg_attach: fast defaults/missing values are not supported")));

        HeapTupleHeaderSetXmin(header, FrozenTransactionId);
        HeapTupleHeaderSetXmax(header, InvalidTransactionId);

        if (target_lz4_supported &&
            (!patch_toast || !(header->t_infomask & HEAP_HASEXTERNAL)))
            continue;

        if (values == NULL)
        {
            values = palloc(sizeof(Datum) * desc->natts);
            isnull = palloc(sizeof(bool) * desc->natts);
        }

        memset(&tuple, 0, sizeof(tuple));
        tuple.t_len = ItemIdGetLength(itemid);
        tuple.t_data = header;
        ItemPointerSetInvalid(&tuple.t_self);
        heap_deform_tuple(&tuple, desc, values, isnull);

        for (attno = 0; attno < desc->natts; attno++)
        {
            Form_pg_attribute attr = TupleDescAttr(desc, attno);
            struct varlena *value;

            if (isnull[attno] || attr->attisdropped || attr->attlen != -1)
                continue;

            value = (struct varlena *) DatumGetPointer(values[attno]);
            if (!target_lz4_supported && VARATT_IS_4B_C(value) &&
                VARDATA_COMPRESSED_GET_COMPRESS_METHOD(value) == TOAST_LZ4_COMPRESSION_ID)
                ereport(ERROR,
                        (errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
                         errmsg("pg_attach: target does not support lz4-compressed inline values")));

            if (VARATT_IS_EXTERNAL_ONDISK(value))
            {
                varatt_external external;

                VARATT_EXTERNAL_GET_POINTER(external, value);
                if (!target_lz4_supported && VARATT_EXTERNAL_IS_COMPRESSED(external) &&
                    VARATT_EXTERNAL_GET_COMPRESS_METHOD(external) == TOAST_LZ4_COMPRESSION_ID)
                    ereport(ERROR,
                            (errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
                             errmsg("pg_attach: target does not support lz4-compressed TOAST values")));
                if (!patch_toast)
                    continue;
                if (target_toast_oid == InvalidOid)
                    ereport(ERROR,
                            (errcode(ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE),
                             errmsg("pg_attach: target table has no TOAST relation")));
                if (external.va_toastrelid != source_toast_oid)
                    ereport(ERROR,
                            (errcode(ERRCODE_DATA_CORRUPTED),
                             errmsg("pg_attach: unexpected TOAST relation OID %u in block %u",
                                    external.va_toastrelid, blkno)));
                external.va_toastrelid = target_toast_oid;
                memcpy(VARDATA_EXTERNAL(value), &external, sizeof(external));
            }
        }
    }

    if (values != NULL)
    {
        pfree(values);
        pfree(isnull);
    }
    pg_attach_finish_page(page, blkno, checksums);
}

Oid
pg_attach_toast_index_oid(Relation toast_rel)
{
    List *indexes = RelationGetIndexList(toast_rel);
    Oid result = list_length(indexes) == 1 ? linitial_oid(indexes) : InvalidOid;

    list_free(indexes);
    return result;
}

int
pg_attach_index_oids(Relation rel, Oid **oids)
{
    List *index_list = RelationGetIndexList(rel);
    int count = list_length(index_list);
    Oid *result = palloc(sizeof(Oid) * (count > 0 ? count : 1));
    ListCell *cell;
    int i = 0;

    foreach(cell, index_list)
        result[i++] = lfirst_oid(cell);
    list_free(index_list);
    *oids = result;
    return count;
}

const char *
pg_attach_index_definition(Oid index_oid)
{
    char *definition = pg_get_indexdef_string(index_oid);
    char *using_btree = strstr(definition, " USING btree ");

    return using_btree == NULL ? definition : using_btree + 7;
}

void
pg_attach_attributes(Relation rel, PgAttachAttribute *attrs)
{
    TupleDesc desc = RelationGetDescr(rel);
    int i;

    for (i = 0; i < desc->natts; i++)
    {
        Form_pg_attribute src = TupleDescAttr(desc, i);
        PgAttachAttribute *dst = &attrs[i];

        memset(dst, 0, sizeof(*dst));
        strlcpy(dst->name, NameStr(src->attname), NAMEDATALEN);
        dst->typmod = src->atttypmod;
        dst->len = src->attlen;
        dst->align = (uint8) src->attalign;
        dst->by_val = src->attbyval;
        dst->storage = (uint8) src->attstorage;
        dst->has_missing = src->atthasmissing;
        dst->generated = (uint8) src->attgenerated;
        dst->not_null = src->attnotnull;
        dst->dropped = src->attisdropped;
    }
}

int
pg_attach_attribute_count(Relation rel)
{
    return RelationGetDescr(rel)->natts;
}

/*
 * Size of an optional fork, 0 when its file does not exist: unlike
 * smgrnblocks this never ereports, which manifest building relies on.
 */
BlockNumber
pg_attach_blocks_safe(Relation rel, ForkNumber fork)
{
    SMgrRelation smgr = RelationGetSmgr(rel);

    if (!smgrexists(smgr, fork))
        return 0;
    return smgrnblocks(smgr, fork);
}

void
pg_attach_write_page(Relation rel, ForkNumber fork, BlockNumber blkno, const char *page)
{
    SMgrRelation smgr = RelationGetSmgr(rel);
    BlockNumber blocks;

    if (!smgrexists(smgr, fork))
        smgrcreate(smgr, fork, false);

    blocks = smgrnblocks(smgr, fork);
    if (blkno == blocks)
        smgrextend(smgr, fork, blkno, page, false);
    else if (blkno < blocks)
        smgrwrite(smgr, fork, blkno, page, false);
    else
        ereport(ERROR,
                (errcode(ERRCODE_DATA_CORRUPTED),
                 errmsg("pg_attach: non-contiguous block %u, relation has %u blocks", blkno, blocks)));
}

/*
 * Emit WAL full-page images for a relation whose pages were written directly
 * through SMGR: reread them without the buffer manager and log them in
 * batches of XLR_MAX_BLOCK_ID, the same batching core's bulk writer uses.
 * No explicit XLogFlush: the caller commits right after, and the commit
 * record's own WAL flush makes these earlier images durable.
 */
void
pg_attach_log_relation(Relation rel, ForkNumber fork)
{
    SMgrRelation smgr = RelationGetSmgr(rel);
    BlockNumber nblocks;
    Page       *pages;
    BlockNumber *blknos;
    BlockNumber blkno;
    int         npending = 0;
    int         i;

    if (!smgrexists(smgr, fork))
        return;
    nblocks = smgrnblocks(smgr, fork);
    if (nblocks == 0)
        return;

    pages = palloc(sizeof(Page) * XLR_MAX_BLOCK_ID);
    blknos = palloc(sizeof(BlockNumber) * XLR_MAX_BLOCK_ID);
    for (i = 0; i < XLR_MAX_BLOCK_ID; i++)
        pages[i] = palloc(BLCKSZ);

    for (blkno = 0; blkno < nblocks; blkno++)
    {
        CHECK_FOR_INTERRUPTS();
        smgrread(smgr, fork, blkno, pages[npending]);
        blknos[npending] = blkno;
        if (++npending == XLR_MAX_BLOCK_ID)
        {
            log_newpages(&smgr->smgr_rlocator.locator, fork, npending,
                         blknos, pages, fork == MAIN_FORKNUM);
            npending = 0;
        }
    }
    if (npending > 0)
        log_newpages(&smgr->smgr_rlocator.locator, fork, npending,
                     blknos, pages, fork == MAIN_FORKNUM);

    for (i = 0; i < XLR_MAX_BLOCK_ID; i++)
        pfree(pages[i]);
    pfree(pages);
    pfree(blknos);

    smgrimmedsync(smgr, fork);
}

void
pg_attach_sync_relation(Relation rel, ForkNumber fork)
{
    SMgrRelation smgr = RelationGetSmgr(rel);

    if (smgrexists(smgr, fork))
        smgrimmedsync(smgr, fork);
}
