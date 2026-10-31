#include "postgres.h"

#include "access/detoast.h"
#include "access/heapam.h"
#include "access/htup_details.h"
#include "access/xlog.h"
#include "access/xloginsert.h"
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

/* C representation shared with wire::Attribute; all fields have fixed widths. */
typedef struct PgAttachAttribute
{
    char        name[NAMEDATALEN];
    Oid         type_oid;
    Oid         collation_oid;
    int32       typmod;
    int16       len;
    bool        not_null;
    bool        dropped;
} PgAttachAttribute;

Oid
pg_attach_resolve_relation(const char *name)
{
    return RangeVarGetRelid(makeRangeVarFromNameList(stringToQualifiedNameList(name, NULL)),
                            NoLock, true);
}

void
pg_attach_read_page(Relation rel, ForkNumber fork, BlockNumber blkno, char *dst)
{
    Buffer buffer = ReadBufferExtended(rel, fork, blkno, RBM_NORMAL, NULL);

    LockBuffer(buffer, BUFFER_LOCK_SHARE);
    memcpy(dst, BufferGetPage(buffer), BLCKSZ);
    UnlockReleaseBuffer(buffer);
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

void
pg_attach_prepare_heap_page(Page page, Relation rel, BlockNumber blkno,
                            Oid source_toast_oid, Oid target_toast_oid,
                            bool patch_toast, bool checksums)
{
    TupleDesc desc = RelationGetDescr(rel);
    Datum *values = palloc(sizeof(Datum) * desc->natts);
    bool *isnull = palloc(sizeof(bool) * desc->natts);
    OffsetNumber offnum;
    OffsetNumber maxoff = PageGetMaxOffsetNumber(page);

    ((PageHeader) page)->pd_prune_xid = 0;

    for (offnum = FirstOffsetNumber; offnum <= maxoff; offnum++)
    {
        ItemId itemid;
        CHECK_FOR_INTERRUPTS();
        itemid = PageGetItemId(page, offnum);
        HeapTupleHeader header;
        HeapTupleData tuple;
        int attno;

        if (!ItemIdIsNormal(itemid))
            continue;

        header = (HeapTupleHeader) PageGetItem(page, itemid);
        if (!HeapTupleHeaderXminFrozen(header) ||
            !(header->t_infomask & HEAP_XMAX_INVALID))
            ereport(ERROR,
                    (errcode(ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE),
                     errmsg("pg_attach: block %u contains a tuple that is not frozen; rerun VACUUM (FREEZE)", blkno)));

        HeapTupleHeaderSetXmin(header, FrozenTransactionId);
        HeapTupleHeaderSetXmax(header, InvalidTransactionId);

        if (!patch_toast)
            continue;

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
            if (VARATT_IS_EXTERNAL_ONDISK(value))
            {
                varatt_external external;

                if (target_toast_oid == InvalidOid)
                    ereport(ERROR,
                            (errcode(ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE),
                             errmsg("pg_attach: target table has no TOAST relation")));

                VARATT_EXTERNAL_GET_POINTER(external, value);
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

    pfree(values);
    pfree(isnull);
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
        dst->type_oid = src->atttypid;
        dst->collation_oid = src->attcollation;
        dst->typmod = src->atttypmod;
        dst->len = src->attlen;
        dst->not_null = src->attnotnull;
        dst->dropped = src->attisdropped;
    }
}

int
pg_attach_attribute_count(Relation rel)
{
    return RelationGetDescr(rel)->natts;
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

void
pg_attach_log_relation(Relation rel, ForkNumber fork)
{
    BlockNumber blocks = RelationGetNumberOfBlocksInFork(rel, fork);

    if (blocks > 0)
        log_newpage_range(rel, fork, 0, blocks, fork == MAIN_FORKNUM);
}

void
pg_attach_sync_relation(Relation rel, ForkNumber fork)
{
    SMgrRelation smgr = RelationGetSmgr(rel);

    if (smgrexists(smgr, fork))
        smgrimmedsync(smgr, fork);
}
