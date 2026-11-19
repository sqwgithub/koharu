import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { expect, it, vi } from 'vitest'

import { GlossaryManager } from '@/components/editor/GlossaryManager'
import type { GlossaryEntry } from '@koharu/ipc/protocol'

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
  const apply = vi.fn(async () => 1)
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
  await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('1'))
  rerender(manager([{ ...entry, excluded_layers: ['keep', 'skip'] }]))
  expect(button()).toBeDisabled()
})
