'use client'

import { useQuery } from '@tanstack/react-query'
import { ArrowUpRight } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import { call } from '@/lib/backend'
import { glossaryKey, refresh, useProject } from '@/lib/queries'
import { rgbaToHex, useKoharuStore } from '@/lib/store'
import {
  commands,
  type InkChoice,
  type InkSuggestion,
  type ProjectId,
  type TermLocation,
} from '@koharu/ipc/protocol'
import { Button } from '@koharu/ui/components/button'
import { Input } from '@koharu/ui/components/input'
import { NativeSelect, NativeSelectOption } from '@koharu/ui/components/native-select'
import { ScrollArea } from '@koharu/ui/components/scroll-area'
import { Textarea } from '@koharu/ui/components/textarea'

export async function showEvidence({ page, layer }: Pick<TermLocation, 'page' | 'layer'>) {
  await call(commands.show, page)
  const { selectPages, selectLayers } = useKoharuStore.getState()
  selectPages([page])
  selectLayers([layer])
  await call(commands.select, [layer])
}

export function ReviewPanel({ project }: { project: ProjectId }) {
  const { t } = useTranslation()
  const { data } = useQuery({
    queryKey: [...glossaryKey, project],
    queryFn: () => call(commands.getGlossary),
  })
  const [edits, setEdits] = useState<Record<string, string>>({})
  const [busy, setBusy] = useState(false)
  const shownPage = useProject().data?.page
  const [inkPage, setInkPage] = useState<string>()
  if (!data || data.project !== project) return null
  const byPage = new Map<string, InkSuggestion[]>()
  for (const ink of data.inks) {
    const page = ink.location.page
    const rows = byPage.get(page)
    if (rows) rows.push(ink)
    else byPage.set(page, [ink])
  }
  const visibleInks =
    byPage.get(inkPage ?? '') ?? byPage.get(shownPage ?? '') ?? byPage.values().next().value ?? []
  const resolveInk = async (ink: InkSuggestion, choice: InkChoice) => {
    setBusy(true)
    try {
      await call(commands.resolveInk, project, ink, choice)
      await refresh(glossaryKey)
    } finally {
      setBusy(false)
    }
  }
  const hiddenInks = visibleInks.filter((ink) => ink.hidden)
  const inkRow = (ink: InkSuggestion) => {
    const color = rgbaToHex(ink.detected)
    const current = ink.translation_fill && rgbaToHex(ink.translation_fill)
    return (
      <article key={ink.location.layer} className='space-y-2 border-b py-2'>
        <button
          type='button'
          className='flex items-baseline gap-1 text-start hover:text-primary'
          onClick={() => void showEvidence(ink.location).catch(() => undefined)}
        >
          <ArrowUpRight className='size-3 shrink-0' />
          {ink.block_number != null && <strong>#{ink.block_number}</strong>}
          <span className='wrap-anywhere'>{ink.location.text || t('layers.kinds.block')}</span>
        </button>
        <div className='flex flex-wrap items-center gap-1.5'>
          <span
            aria-hidden
            className='size-3 rounded-sm border'
            style={{ backgroundColor: color }}
          />
          <code>{color}</code>
          {current && current !== color && (
            <span>
              {t('glossary.translation')} · {current}
            </span>
          )}
        </div>
        <div className='flex flex-wrap gap-1'>
          {(['keep', 'black', 'white'] as const).map((choice) => (
            <Button
              key={choice}
              size='xs'
              variant='outline'
              disabled={busy}
              onClick={() => void resolveInk(ink, choice).catch(() => undefined)}
            >
              {t(`review.inkChoice.${choice}`)}
            </Button>
          ))}
        </div>
      </article>
    )
  }
  const items = [
    ...(data.candidates ?? []).flatMap(({ pending, entries, locations }) => {
      const entry = entries[0]
      if (!pending || !entry) return []
      return [
        {
          id: JSON.stringify(['term', entry]),
          title: t('review.term', { source: entry.source }),
          reason: entry.note,
          translation: entry.translation || entry.source,
          locations: locations.slice(0, 1),
          previous: '',
          candidate: null,
          confirmOnly: false,
          resolve: (translation: string | null) =>
            call(commands.edit, {
              label: 'edit_glossary',
              ops: [{ op: 'resolve_glossary_term', project, candidate: entry, translation }],
            }),
        },
      ]
    }),
    ...data.candidate_reviews.map((suggestion) => ({
      id: JSON.stringify(['candidate', suggestion]),
      title: t('review.candidate', { source: suggestion.source, expected: suggestion.expected }),
      reason:
        !suggestion.reason.trim() || suggestion.reason === 'glossary_location_conflict'
          ? t('review.locationConflict')
          : suggestion.reason,
      translation: suggestion.preview ?? suggestion.previous,
      locations: [suggestion.location],
      previous: suggestion.preview ? suggestion.previous : '',
      candidate: suggestion,
      confirmOnly: false,
      resolve: (value: string | null) =>
        call(commands.resolveCandidateReview, project, suggestion, value),
    })),
    ...data.uncertainties.map((suggestion) => ({
      id: JSON.stringify([
        'uncertainty',
        suggestion.location.layer,
        suggestion.location.text,
        suggestion.translation,
        suggestion.reason,
      ]),
      title: t('review.uncertainTranslation'),
      reason: suggestion.reason,
      translation: suggestion.translation,
      locations: [suggestion.location],
      previous: '',
      candidate: null,
      confirmOnly: true,
      resolve: (value: string | null) =>
        call(commands.resolveUncertainty, project, suggestion, value ?? suggestion.translation),
    })),
    ...data.suggestions.map((suggestion) => ({
      id: JSON.stringify(['alias', suggestion.source, suggestion.canonical]),
      title: t('review.alias', suggestion),
      reason: suggestion.reason,
      translation: suggestion.translation,
      locations: suggestion.locations,
      previous: '',
      candidate: null,
      confirmOnly: false,
      resolve: (value: string | null) => call(commands.resolveAlias, project, suggestion, value),
    })),
    ...data.corrections.map((correction) => ({
      id: JSON.stringify([
        'translation',
        correction.location.layer,
        correction.previous,
        correction.translation,
      ]),
      title: t('review.correction'),
      reason: correction.reason,
      translation: correction.translation,
      locations: [correction.location],
      previous: correction.previous,
      candidate: null,
      confirmOnly: false,
      resolve: (value: string | null) =>
        call(commands.resolveTranslation, project, correction, value),
    })),
  ]

  return (
    <ScrollArea className='h-full min-h-0 flex-1'>
      {!items.length && !data.inks.length && (
        <p role='status' className='p-3 text-xs text-muted-foreground'>
          {t('review.empty')}
        </p>
      )}
      {items.map((item) => {
        const draft = edits[item.id] ?? item.translation
        const candidate = item.candidate
        const TranslationField = candidate ? Textarea : Input
        const resolve = async (value: string | null) => {
          setBusy(true)
          try {
            await item.resolve(value)
            setEdits((current) => {
              if (!(item.id in current)) return current
              const next = { ...current }
              delete next[item.id]
              return next
            })
            await refresh(glossaryKey)
          } finally {
            setBusy(false)
          }
        }
        return (
          <section key={item.id} className='space-y-2 border-b p-3 text-xs'>
            <h3 className='font-semibold wrap-anywhere'>{item.title}</h3>
            {item.previous && (
              <p className='wrap-anywhere whitespace-pre-wrap'>
                {candidate && <strong>{t('review.currentTranslation')} · </strong>}
                {item.previous}
              </p>
            )}
            {item.reason && <p className='wrap-anywhere text-muted-foreground'>{item.reason}</p>}
            <ul className='space-y-1'>
              {item.locations.map((location) => (
                <li key={location.layer}>
                  <button
                    type='button'
                    className='flex w-full items-baseline gap-1 rounded text-start hover:text-primary'
                    onClick={() => void showEvidence(location).catch(() => undefined)}
                  >
                    <ArrowUpRight className='size-3 shrink-0' />
                    <span className='wrap-anywhere'>
                      {t('glossary.page', { page: location.number })} · {location.text}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
            <label className='flex items-center gap-2'>
              <span className='shrink-0'>
                {t(
                  candidate
                    ? candidate.preview
                      ? 'review.proposedEdit'
                      : 'review.currentTranslation'
                    : 'glossary.translation',
                )}
              </span>
              <TranslationField
                value={draft}
                onChange={(event) => setEdits({ ...edits, [item.id]: event.target.value })}
              />
            </label>
            <div className='flex gap-2'>
              <Button
                size='xs'
                disabled={busy || !draft.trim() || draft === candidate?.previous}
                onClick={() =>
                  void resolve(candidate ? draft : draft.trim()).catch(() => undefined)
                }
              >
                {t(
                  candidate
                    ? 'review.saveEdit'
                    : item.confirmOnly
                      ? 'review.confirmTranslation'
                      : 'review.accept',
                )}
              </Button>
              {!item.confirmOnly && (
                <Button
                  size='xs'
                  variant='outline'
                  disabled={busy}
                  onClick={() => void resolve(null).catch(() => undefined)}
                >
                  {t(candidate ? 'review.keepTranslation' : 'review.dismiss')}
                </Button>
              )}
            </div>
          </section>
        )
      })}
      {visibleInks.length > 0 && (
        <section className='space-y-2 p-3 text-xs'>
          <label className='flex flex-wrap items-center gap-2 font-semibold'>
            {t('review.ink')}
            <NativeSelect
              value={visibleInks[0]!.location.page}
              onChange={(e) => setInkPage(e.target.value)}
            >
              {Array.from(byPage, ([page, rows]) => (
                <NativeSelectOption key={page} value={page}>
                  {t('glossary.page', { page: rows[0]!.location.number })} ({rows.length})
                </NativeSelectOption>
              ))}
            </NativeSelect>
          </label>
          {visibleInks.filter((ink) => !ink.hidden).map(inkRow)}
          {hiddenInks.length > 0 && (
            <details key={visibleInks[0]!.location.page}>
              <summary className='cursor-pointer py-1 text-muted-foreground'>
                {t('ink.hidden', { count: hiddenInks.length })}
              </summary>
              {hiddenInks.map(inkRow)}
            </details>
          )}
        </section>
      )}
    </ScrollArea>
  )
}
