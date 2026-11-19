import { QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { expect, it, vi } from 'vitest'

import { GlossaryManager } from '@/components/editor/GlossaryManager'
import { ReviewPanel } from '@/components/editor/ReviewPanel'
import { glossaryKey, projectKey, queryClient } from '@/lib/queries'
import { commands, type GlossaryEntry } from '@koharu/ipc/protocol'

vi.mock('@koharu/ipc/protocol', async (load) => {
  const protocol = await load<typeof import('@koharu/ipc/protocol')>()
  return {
    ...protocol,
    commands: {
      ...protocol.commands,
      show: vi.fn(),
      select: vi.fn(),
      resolveCandidateReview: vi.fn(),
      getGlossary: vi.fn(async () => null),
    },
  }
})

it('applies only the selected candidate to its included occurrences', async () => {
  const user = userEvent.setup()
  const entry: GlossaryEntry = {
    source: 'お兄さん',
    translation: '哥哥',
    note: '',
    kind: 'term',
    excluded_layers: ['skip'],
  }
  const group = {
    entries: [entry],
    manual: [],
    locations: ['keep', 'skip'].map((layer, i) => ({
      layer,
      number: i + 1,
      page: 'page',
      text: entry.source,
    })),
  }
  let complete!: (value: { updated: number; review: number }) => void
  const apply = vi.fn(
    () =>
      new Promise<{ updated: number; review: number }>((resolve) => {
        complete = resolve
      }),
  )
  const manager = (entries: GlossaryEntry[]) => (
    <GlossaryManager
      entries={entries}
      candidates={[group]}
      onSave={vi.fn()}
      onApplyCandidate={apply}
    />
  )
  const { rerender } = render(manager([]))
  await user.click(screen.getByRole('tab', { name: 'Terminology candidates' }))
  await user.click(screen.getByText(entry.source))
  const button = () => screen.getByRole('button', { name: 'Apply to existing translations' })
  expect(button()).toBeDisabled()
  rerender(manager([entry]))
  await user.click(button())
  expect(apply).toHaveBeenCalledWith(entry, ['keep'])
  expect(screen.getByRole('status')).toHaveTextContent('Locating terms')
  expect(button()).toBeDisabled()
  complete({ updated: 1, review: 2 })
  await waitFor(() =>
    expect(screen.getByRole('status')).toHaveTextContent(
      'Updated text blocks: 1. Flagged for review: 2.',
    ),
  )
  rerender(manager([{ ...entry, excluded_layers: ['keep', 'skip'] }]))
  expect(button()).toBeDisabled()
})

it.each([false, true])(
  'reviews a candidate against the full translation with preview = %s',
  async (hasPreview) => {
    const user = userEvent.setup()
    const suggestion = {
      location: { layer: 'block', page: 'page', number: 5, text: 'れあ？' },
      source: 'れあ',
      expected: '玲爱',
      previous: '蕾恩？',
      preview: hasPreview ? '玲爱？' : null,
      reason: hasPreview ? 'glossary_location_conflict' : '',
    }
    queryClient.setQueryData([...glossaryKey, 'book'], {
      project: 'book',
      uncertainties: [],
      suggestions: [],
      corrections: [],
      inks: [],
      candidate_reviews: [suggestion],
    })
    queryClient.setQueryData(projectKey, null)
    render(
      <QueryClientProvider client={queryClient}>
        <ReviewPanel project='book' />
      </QueryClientProvider>,
    )
    expect(screen.getByText(/could not be safely replaced/)).toBeVisible()
    const save = screen.getByRole('button', { name: 'Save edit' })
    const field = screen.getByRole('textbox', {
      name: hasPreview ? 'Proposed edit' : 'Current translation',
    })
    expect(field).toHaveValue(suggestion.preview ?? suggestion.previous)
    if (hasPreview) {
      expect(save).toBeEnabled()
      await user.click(screen.getByRole('button', { name: 'Keep translation' }))
      await waitFor(() =>
        expect(commands.resolveCandidateReview).toHaveBeenCalledWith('book', suggestion, null),
      )
    } else {
      expect(save).toBeDisabled()
      await user.click(screen.getByRole('button', { name: /れあ？/ }))
      expect(commands.show).toHaveBeenCalledWith('page')
      expect(commands.select).toHaveBeenCalledWith(['block'])
      const edited = '是玲爱\n吗？'
      fireEvent.change(field, { target: { value: edited } })
      await user.click(save)
      await waitFor(() =>
        expect(commands.resolveCandidateReview).toHaveBeenCalledWith('book', suggestion, edited),
      )
    }
  },
)
