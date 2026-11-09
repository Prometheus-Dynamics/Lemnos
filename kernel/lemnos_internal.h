/* SPDX-License-Identifier: GPL-2.0 */
/* What the module's files share. Copyright (c) 2026 the Lemnos authors. */
#ifndef LEMNOS_INTERNAL_H
#define LEMNOS_INTERNAL_H

#include "lemnos.h"

long lem_claim(struct lemnos_claim *c);

struct device;

long lem_stream_create(struct device *dev, struct lemnos_stream_create *c);
void lem_streams_block_gone(struct device *dev);

#endif
