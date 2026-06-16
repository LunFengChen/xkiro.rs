import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  getProxies,
  addProxy,
  updateProxy,
  deleteProxy,
  testProxy,
  importProxies,
  autoAssignProxies,
  setCredentialProxy,
} from '@/api/proxies'
import { usePageActive } from '@/hooks/use-page-active'
import type {
  ProxyUpsertRequest,
  ProxyImportRequest,
  ProxyAutoAssignRequest,
} from '@/types/api'

// 查询代理列表
export function useProxies() {
  const pageActive = usePageActive()
  return useQuery({
    queryKey: ['proxies'],
    queryFn: getProxies,
    // 仅在用户停留前端时周期刷新；离开后停止
    refetchInterval: pageActive ? 30000 : false,
    refetchIntervalInBackground: false,
  })
}

// 新增代理
export function useAddProxy() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (req: ProxyUpsertRequest) => addProxy(req),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
    },
  })
}

// 更新代理
export function useUpdateProxy() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, req }: { id: number; req: ProxyUpsertRequest }) =>
      updateProxy(id, req),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
    },
  })
}

// 删除代理（会自动解绑凭据，故同时刷新 credentials）
export function useDeleteProxy() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => deleteProxy(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 测试代理连通性（不改数据，故不需 invalidate）
export function useTestProxy() {
  return useMutation({
    mutationFn: (id: number) => testProxy(id),
  })
}

// 批量导入代理
export function useImportProxies() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (req: ProxyImportRequest) => importProxies(req),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
    },
  })
}

// 自动分配代理（同时影响凭据绑定）
export function useAutoAssignProxies() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (req: ProxyAutoAssignRequest) => autoAssignProxies(req),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 绑定/解绑凭据到代理
export function useSetCredentialProxy() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, proxyId }: { id: number; proxyId: number | null }) =>
      setCredentialProxy(id, proxyId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
    },
  })
}
