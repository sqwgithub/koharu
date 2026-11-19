import { QueryClientProvider } from '@tanstack/react-query'
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { expect, it, vi } from 'vitest'

import { NormalizeInkDialog } from '@/components/editor/NormalizeInkDialog'
import { ReviewPanel } from '@/components/editor/ReviewPanel'
import { glossaryKey, projectKey, queryClient } from '@/lib/queries'
import { commands } from '@koharu/ipc/protocol'

vi.mock('@koharu/ipc/protocol', async (load) => {
  const protocol = await load<typeof import('@koharu/ipc/protocol')>()
  return { ...protocol, commands: { ...protocol.commands, normalizeInk: vi.fn(async () => 2) } }
})

it('validates the channel ranges and restores independently of the custom settings', async () => {
  render(<NormalizeInkDialog project='book' pageCount={2} onClose={vi.fn()} />)
  fireEvent.click(screen.getByText('Advanced options'))
  const black = screen.getByRole('spinbutton', { name: /Near-black/ })
  const apply = screen.getByRole('button', { name: 'Normalize colors' })
  fireEvent.change(black, { target: { value: '231' } })
  expect(apply).toBeDisabled()
  expect(screen.getByRole('alert')).toBeVisible()
  fireEvent.change(black, { target: { value: '16' } })
  fireEvent.click(apply)
  await waitFor(() =>
    expect(commands.normalizeInk).toHaveBeenCalledWith('book', {
      black_max: 16,
      white_min: 231,
      black: [0, 0, 0, 1],
      white: [1, 1, 1, 1],
    }),
  )
  await screen.findByText('Text blocks changed: 2')
  fireEvent.change(black, { target: { value: '' } })
  fireEvent.click(screen.getByRole('button', { name: 'Restore detected colors' }))
  await waitFor(() => expect(commands.normalizeInk).toHaveBeenLastCalledWith('book', null))
})

it('moves pending rows as their effective visibility changes', async () => {
  const rows = (hidden: boolean) => ({
    project: 'book',
    suggestions: [],
    uncertainties: [],
    candidate_reviews: [],
    corrections: [],
    inks: [
      {
        hidden,
        location: { layer: 'block', page: 'page', number: 1, text: 'source' },
        detected: [0.5, 0.5, 0.5, 1],
        translation_fill: null,
        block_number: null,
      },
    ],
  })
  const key = [...glossaryKey, 'book']
  queryClient.setQueryData(key, rows(true))
  queryClient.setQueryData(projectKey, null)
  render(
    <QueryClientProvider client={queryClient}>
      <ReviewPanel project='book' />
    </QueryClientProvider>,
  )
  expect(screen.getByText('source').closest('details')).not.toHaveAttribute('open')
  await act(async () => {
    queryClient.setQueryData(key, rows(false))
  })
  await waitFor(() => expect(screen.getByText('source').closest('details')).toBeNull())
  await act(async () => {
    queryClient.setQueryData(key, rows(true))
  })
  await waitFor(() =>
    expect(screen.getByText('source').closest('details')).toBeInstanceOf(HTMLDetailsElement),
  )
  expect(screen.getByText('source').closest('details')).not.toHaveAttribute('open')
})
