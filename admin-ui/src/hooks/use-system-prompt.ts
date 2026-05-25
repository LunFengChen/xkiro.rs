import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  getSystemPrompt,
  updateSystemPrompt,
  upsertUserPreset,
  deleteUserPreset,
} from '@/api/credentials'
import type {
  UpdateSystemPromptRequest,
  UpsertUserPresetRequest,
} from '@/types/api'

const KEY = ['system-prompt'] as const

export function useSystemPrompt(enabled = true) {
  return useQuery({
    queryKey: KEY,
    queryFn: getSystemPrompt,
    enabled,
    staleTime: 5_000,
  })
}

export function useUpdateSystemPrompt() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (req: UpdateSystemPromptRequest) => updateSystemPrompt(req),
    onSuccess: (data) => {
      qc.setQueryData(KEY, data)
    },
  })
}

export function useUpsertUserPreset() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (req: UpsertUserPresetRequest) => upsertUserPreset(req),
    onSuccess: (data) => {
      qc.setQueryData(KEY, data)
    },
  })
}

export function useDeleteUserPreset() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => deleteUserPreset(id),
    onSuccess: (data) => {
      qc.setQueryData(KEY, data)
    },
  })
}
