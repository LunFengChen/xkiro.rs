import axios from 'axios'
import { storage } from '@/lib/storage'
import type {
  ProxyListResponse,
  ProxyUpsertRequest,
  ProxyTestResponse,
  ProxyImportRequest,
  ProxyImportResponse,
  ProxyAutoAssignRequest,
  ProxyAutoAssignResponse,
  SetCredentialProxyRequest,
  SuccessResponse,
} from '@/types/api'

// 创建 axios 实例（与 credentials.ts 同样的拦截器模式）
const api = axios.create({
  baseURL: '/api/admin',
  headers: {
    'Content-Type': 'application/json',
  },
})

// 请求拦截器添加 API Key
api.interceptors.request.use((config) => {
  const apiKey = storage.getApiKey()
  if (apiKey) {
    config.headers['x-api-key'] = apiKey
  }
  return config
})

// 获取所有代理
export async function getProxies(): Promise<ProxyListResponse> {
  const { data } = await api.get<ProxyListResponse>('/proxies')
  return data
}

// 新增代理
export async function addProxy(req: ProxyUpsertRequest): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>('/proxies', req)
  return data
}

// 更新代理
export async function updateProxy(
  id: number,
  req: ProxyUpsertRequest,
): Promise<SuccessResponse> {
  const { data } = await api.put<SuccessResponse>(`/proxies/${id}`, req)
  return data
}

// 删除代理（自动解绑关联凭据）
export async function deleteProxy(id: number): Promise<SuccessResponse> {
  const { data } = await api.delete<SuccessResponse>(`/proxies/${id}`)
  return data
}

// 测试代理连通性
export async function testProxy(id: number): Promise<ProxyTestResponse> {
  const { data } = await api.post<ProxyTestResponse>(`/proxies/${id}/test`)
  return data
}

// 批量导入代理
export async function importProxies(
  req: ProxyImportRequest,
): Promise<ProxyImportResponse> {
  const { data } = await api.post<ProxyImportResponse>('/proxies/import', req)
  return data
}

// 自动分配代理
export async function autoAssignProxies(
  req: ProxyAutoAssignRequest,
): Promise<ProxyAutoAssignResponse> {
  const { data } = await api.post<ProxyAutoAssignResponse>('/proxies/auto-assign', req)
  return data
}

// 绑定/解绑凭据到代理（proxyId = null 解绑）
export async function setCredentialProxy(
  id: number,
  proxyId: number | null,
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/${id}/proxy`,
    { proxyId } as SetCredentialProxyRequest,
  )
  return data
}
