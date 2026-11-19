import { QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { expect, it, vi } from 'vitest'

import { PipelinePreferences } from '@/components/preferences/PipelinePreferences'
import { SoundEffectGlossary } from '@/components/preferences/SoundEffectGlossary'
import { queryClient } from '@/lib/queries'
import { commands } from '@koharu/ipc/protocol'

it('enables fast translation without altering the model selection', () => {
  const onChange = vi.fn()
  render(
    <PipelinePreferences
      value={{ translator: { model: 'test/model' } }}
      models={[]}
      onChange={onChange}
    />,
  )
  const toggle = screen.getByRole('switch', { name: 'Fast translation' })
  expect(toggle).not.toBeChecked()
  fireEvent.click(toggle)
  expect(onChange).toHaveBeenCalledWith({ translator: { model: 'test/model', fast_sfx: true } })
})

it('edits only the selected language pair for a shared source spelling', async () => {
  vi.spyOn(commands, 'getSfxGlossary').mockResolvedValue({
    languages: {
      'ja → zh-CN': { ドン: '咚' },
      'ja → en': { ドン: 'thud' },
    },
  })
  const edit = vi.spyOn(commands, 'editSfxTerm').mockResolvedValue(null)
  render(
    <QueryClientProvider client={queryClient}>
      <SoundEffectGlossary />
    </QueryClientProvider>,
  )
  const row = (await screen.findByText('ja → zh-CN')).parentElement!
  fireEvent.click(within(row).getByRole('button'))
  fireEvent.change(within(row).getByRole('textbox'), { target: { value: '砰' } })
  fireEvent.click(within(row).getByRole('button', { name: 'Save changes' }))
  await waitFor(() => expect(edit).toHaveBeenCalledWith('ja → zh-CN', 'ドン', '咚', '砰'))
})
