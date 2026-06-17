import { useState, useEffect, useRef } from 'react'
import { RefreshCw, LogOut, Moon, Sun, Server, Plus, Upload, FileUp, Trash2, RotateCcw, CheckCircle2, Settings, ZoomIn, FileText, Download, Network } from 'lucide-react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { storage } from '@/lib/storage'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { CredentialCard } from '@/components/credential-card'
import { BalanceDialog } from '@/components/balance-dialog'
import { ModelsDialog } from '@/components/models-dialog'
import { AddCredentialDialog } from '@/components/add-credential-dialog'
import { BatchImportDialog } from '@/components/batch-import-dialog'
import { KamImportDialog } from '@/components/kam-import-dialog'
import { ImportJobToast } from '@/components/import-job-toast'
import { BatchVerifyDialog, type VerifyResult } from '@/components/batch-verify-dialog'
import { SettingsDialog } from '@/components/settings-dialog'
import { SystemPromptDialog } from '@/components/system-prompt-dialog'
import { ProxyPoolDialog } from '@/components/proxy-pool-dialog'
import { useCredentials, useDeleteCredential, useResetFailure, useDisableBatch } from '@/hooks/use-credentials'
import { useRuntimeStats } from '@/hooks/use-runtime-stats'
import { useUiScale } from '@/hooks/use-ui-scale'
import { getCredentialBalance, refreshBatch, refreshBalancesBatch, getCachedBalances, exportTokenJson, exportKam, deleteCredentialsBatch } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { BalanceResponse } from '@/types/api'

interface DashboardProps {
  onLogout: () => void
}

export function Dashboard({ onLogout }: DashboardProps) {
  const [selectedCredentialId, setSelectedCredentialId] = useState<number | null>(null)
  const [balanceDialogOpen, setBalanceDialogOpen] = useState(false)
  const [modelsDialogOpen, setModelsDialogOpen] = useState(false)
  const [addDialogOpen, setAddDialogOpen] = useState(false)
  const [batchImportDialogOpen, setBatchImportDialogOpen] = useState(false)
  const [kamImportDialogOpen, setKamImportDialogOpen] = useState(false)
  const [activeImportJob, setActiveImportJob] = useState<{ jobId: string; total: number } | null>(null)
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set())
  const [verifyDialogOpen, setVerifyDialogOpen] = useState(false)
  const [settingsDialogOpen, setSettingsDialogOpen] = useState(false)
  const [systemPromptDialogOpen, setSystemPromptDialogOpen] = useState(false)
  const [proxyPoolDialogOpen, setProxyPoolDialogOpen] = useState(false)
  const [verifying, setVerifying] = useState(false)
  const [verifyProgress, setVerifyProgress] = useState({ current: 0, total: 0 })
  const [verifyResults, setVerifyResults] = useState<Map<number, VerifyResult>>(new Map())
  const [balanceMap, setBalanceMap] = useState<Map<number, BalanceResponse>>(new Map())
  const [loadingBalanceIds, setLoadingBalanceIds] = useState<Set<number>>(new Set())
  const [queryingInfo, setQueryingInfo] = useState(false)
  const [queryInfoProgress, setQueryInfoProgress] = useState({ current: 0, total: 0 })
  const [batchRefreshing, setBatchRefreshing] = useState(false)
  const [batchRefreshProgress, setBatchRefreshProgress] = useState({ current: 0, total: 0 })
  const [batchQueryingBalance, setBatchQueryingBalance] = useState(false)
  const [batchQueryBalanceProgress, setBatchQueryBalanceProgress] = useState({ current: 0, total: 0 })
  const cancelVerifyRef = useRef(false)
  const [currentPage, setCurrentPage] = useState(1)
  const [itemsPerPage, setItemsPerPage] = useState(() => {
    try {
      const v = parseInt(localStorage.getItem('kiro-page-size') || '', 10)
      return [12, 24, 48, 96, 200].includes(v) ? v : 12
    } catch { return 12 }
  })
  const [compactMode, setCompactMode] = useState(() => {
    try { return localStorage.getItem('kiro-compact-mode') === '1' } catch { return false }
  })
  const [darkMode, setDarkMode] = useState(() => {
    const saved = storage.getTheme()
    if (saved) {
      return saved === 'dark'
    }
    if (typeof window !== 'undefined') {
      return window.matchMedia('(prefers-color-scheme: dark)').matches
    }
    return false
  })
  const { scale: uiScale, setScale: setUiScale, scales: uiScales } = useUiScale()

  const queryClient = useQueryClient()
  const { data, isLoading, error, refetch } = useCredentials()
  const { mutate: deleteCredential } = useDeleteCredential()
  const { mutate: resetFailure } = useResetFailure()
  const { mutateAsync: disableBatch } = useDisableBatch()
  const { data: runtimeMap } = useRuntimeStats()

  // 按组/渠道过滤
  const [filterGroup, setFilterGroup] = useState<string>('')
  const [filterSource, setFilterSource] = useState<string>('')

  // 计算分页（含 group/source 过滤）
  const allCredentials = data?.credentials || []
  const filteredCredentials = allCredentials.filter(c => {
    if (filterGroup && (c.group ?? '') !== filterGroup) return false
    if (filterSource && (c.source ?? '') !== filterSource) return false
    return true
  })
  const totalPages = Math.ceil(filteredCredentials.length / itemsPerPage)
  const startIndex = (currentPage - 1) * itemsPerPage
  const endIndex = startIndex + itemsPerPage
  // 切片后逐元素 merge runtimeMap 的实时字段（K/N + lastUsedAt + disabled）
  const currentCredentials = filteredCredentials.slice(startIndex, endIndex).map(credential => {
    const runtime = runtimeMap?.get(credential.id)
    if (!runtime) return credential
    return {
      ...credential,
      lastUsedAt: runtime.lastUsedAt,
      availablePermits: runtime.availablePermits,
      maxPermits: runtime.maxPermits,
      disabled: runtime.disabled,
    }
  })
  const disabledCredentialCount = allCredentials.filter(credential => credential.disabled).length || 0

  // 所有不重复的 group / source 值（用于过滤下拉）
  const allGroups = [...new Set(allCredentials.map(c => c.group).filter(Boolean) as string[])].sort()
  const allSources = [...new Set(allCredentials.map(c => c.source).filter(Boolean) as string[])].sort()

  // 当凭据列表变化时重置到第一页
  useEffect(() => {
    setCurrentPage(1)
  }, [data?.credentials.length])

  // 每页条数持久化 + 改变时回到第一页
  const handlePageSizeChange = (size: number) => {
    setItemsPerPage(size)
    setCurrentPage(1)
    try { localStorage.setItem('kiro-page-size', String(size)) } catch { /* ignore */ }
  }

  // 只保留当前仍存在的凭据缓存，避免删除后残留旧数据
  useEffect(() => {
    if (!data?.credentials) {
      setBalanceMap(new Map())
      setLoadingBalanceIds(new Set())
      return
    }

    const validIds = new Set(data.credentials.map(credential => credential.id))

    setBalanceMap(prev => {
      const next = new Map<number, BalanceResponse>()
      prev.forEach((value, id) => {
        if (validIds.has(id)) {
          next.set(id, value)
        }
      })
      return next.size === prev.size ? prev : next
    })

    setLoadingBalanceIds(prev => {
      if (prev.size === 0) {
        return prev
      }
      const next = new Set<number>()
      prev.forEach(id => {
        if (validIds.has(id)) {
          next.add(id)
        }
      })
      return next.size === prev.size ? prev : next
    })
  }, [data?.credentials])

  // 初始化时应用主题
  useEffect(() => {
    if (darkMode) {
      document.documentElement.classList.add('dark')
    } else {
      document.documentElement.classList.remove('dark')
    }
  }, [])

  // 首次挂载拉取后端缓存余额，预填到 balanceMap
  // 后端启动时会并行预取所有未禁用凭据的余额并写入磁盘缓存，
  // 这里直接复用，省掉用户进入页面后再手动点查询的步骤
  useEffect(() => {
    let cancelled = false
    getCachedBalances()
      .then(resp => {
        if (cancelled) return
        setBalanceMap(prev => {
          const next = new Map(prev)
          resp.balances.forEach(item => {
            // 把 CachedBalanceItem 投影到 BalanceResponse 形状（字段一一对应）
            next.set(item.id, {
              id: item.id,
              subscriptionTitle: item.subscriptionTitle,
              currentUsage: item.currentUsage,
              usageLimit: item.usageLimit,
              remaining: item.remaining,
              usagePercentage: item.usagePercentage,
              nextResetAt: item.nextResetAt,
              overageCap: item.overageCap,
              overageCapability: item.overageCapability,
              overageStatus: item.overageStatus,
            })
          })
          return next
        })
      })
      .catch(() => {
        // 缓存接口失败不打扰用户，让 dashboard 走原本的手动查询路径
      })
    return () => {
      cancelled = true
    }
  }, [])

  // 把 runtime-stats（1s 轮询）里嵌的余额投影到 balanceMap，实现实时显示
  // 后端余额来自 5min disk cache + 周期后台刷新；前端只负责消费快照
  useEffect(() => {
    if (!runtimeMap || runtimeMap.size === 0) return
    setBalanceMap(prev => {
      let mutated = false
      const next = new Map(prev)
      runtimeMap.forEach((runtime, id) => {
        if (!runtime.balance) return
        const existing = prev.get(id)
        // 浅比较关键字段，避免无变化时触发卡片重渲染
        if (
          existing
          && existing.currentUsage === runtime.balance.currentUsage
          && existing.usageLimit === runtime.balance.usageLimit
          && existing.remaining === runtime.balance.remaining
          && existing.overageStatus === runtime.balance.overageStatus
          && existing.overageCap === runtime.balance.overageCap
        ) {
          return
        }
        next.set(id, {
          id,
          subscriptionTitle: runtime.balance.subscriptionTitle,
          currentUsage: runtime.balance.currentUsage,
          usageLimit: runtime.balance.usageLimit,
          remaining: runtime.balance.remaining,
          usagePercentage: runtime.balance.usagePercentage,
          nextResetAt: runtime.balance.nextResetAt,
          overageCap: runtime.balance.overageCap,
          overageCapability: runtime.balance.overageCapability,
          overageStatus: runtime.balance.overageStatus,
        })
        mutated = true
      })
      return mutated ? next : prev
    })
  }, [runtimeMap])

  const toggleDarkMode = () => {
    const next = !darkMode
    setDarkMode(next)
    storage.setTheme(next ? 'dark' : 'light')
    document.documentElement.classList.toggle('dark')
  }

  const handleViewBalance = (id: number) => {
    setSelectedCredentialId(id)
    setBalanceDialogOpen(true)
  }

  const handleViewModels = (id: number) => {
    setSelectedCredentialId(id)
    setModelsDialogOpen(true)
  }

  const handleRefresh = () => {
    refetch()
    toast.success('已刷新凭据列表')
  }

  const handleLogout = () => {
    storage.removeApiKey()
    queryClient.clear()
    onLogout()
  }

  // 选择管理
  const toggleSelect = (id: number) => {
    const newSelected = new Set(selectedIds)
    if (newSelected.has(id)) {
      newSelected.delete(id)
    } else {
      newSelected.add(id)
    }
    setSelectedIds(newSelected)
  }

  const deselectAll = () => {
    setSelectedIds(new Set())
  }

  // 批量删除（任意状态可删）
  const handleBatchDelete = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要删除的凭据')
      return
    }

    const ids = Array.from(selectedIds)
    if (!confirm(`确定要删除选中的 ${ids.length} 个凭据吗？此操作无法撤销（系统每 6 小时自动备份）。`)) {
      return
    }

    try {
      const res = await deleteCredentialsBatch(ids)
      if (res.failureCount === 0) {
        toast.success(`成功删除 ${res.successCount} 个凭据`)
      } else {
        toast.warning(`删除：成功 ${res.successCount}，失败 ${res.failureCount}`)
      }
    } catch (err) {
      toast.error(`批量删除失败: ${extractErrorMessage(err)}`)
    }

    deselectAll()
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
  }

  // 批量禁用 / 批量启用
  const handleBatchDisable = async (disabled: boolean) => {
    if (selectedIds.size === 0) {
      toast.error(`请先选择凭据`)
      return
    }
    const ids = Array.from(selectedIds)
    const label = disabled ? '禁用' : '启用'
    try {
      const res = await disableBatch({ ids, disabled })
      if (res.failureCount === 0) {
        toast.success(`成功${label} ${res.successCount} 个凭据`)
      } else {
        toast.warning(`${label}：成功 ${res.successCount}，失败 ${res.failureCount}`)
      }
    } catch (err) {
      toast.error(`批量${label}失败: ${extractErrorMessage(err)}`)
    }
    deselectAll()
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
  }

  // 批量恢复异常
  const handleBatchResetFailure = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要恢复的凭据')
      return
    }

    const failedIds = Array.from(selectedIds).filter(id => {
      const cred = data?.credentials.find(c => c.id === id)
      return cred && cred.failureCount > 0
    })

    if (failedIds.length === 0) {
      toast.error('选中的凭据中没有失败的凭据')
      return
    }

    let successCount = 0
    let failCount = 0

    for (const id of failedIds) {
      try {
        await new Promise<void>((resolve, reject) => {
          resetFailure(id, {
            onSuccess: () => {
              successCount++
              resolve()
            },
            onError: (err) => {
              failCount++
              reject(err)
            }
          })
        })
      } catch (error) {
        // 错误已在 onError 中处理
      }
    }

    if (failCount === 0) {
      toast.success(`成功恢复 ${successCount} 个凭据`)
    } else {
      toast.warning(`成功 ${successCount} 个，失败 ${failCount} 个`)
    }

    deselectAll()
  }

  // 批量刷新 Token
  const handleBatchForceRefresh = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要刷新的凭据')
      return
    }

    const enabledIds = Array.from(selectedIds).filter(id => {
      const cred = data?.credentials.find(c => c.id === id)
      return cred && !cred.disabled
    })

    if (enabledIds.length === 0) {
      toast.error('选中的凭据中没有启用的凭据')
      return
    }

    setBatchRefreshing(true)
    setBatchRefreshProgress({ current: 0, total: enabledIds.length })

    try {
      const resp = await refreshBatch(enabledIds)
      setBatchRefreshProgress({ current: enabledIds.length, total: enabledIds.length })

      if (resp.failureCount === 0) {
        toast.success(`成功刷新 ${resp.successCount} 个凭据的 Token`)
      } else {
        toast.warning(`刷新 Token：成功 ${resp.successCount} 个，失败 ${resp.failureCount} 个`)
      }
    } catch (error) {
      toast.error(`批量刷新失败：${extractErrorMessage(error)}`)
    } finally {
      setBatchRefreshing(false)
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    }

    deselectAll()
  }

  // 批量查询余额（服务端 Semaphore(8) 并发，前端一次往返；成功项回填到 balanceMap 复用 BalanceDialog 数据）
  const handleBatchQueryBalance = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要查询的凭据')
      return
    }

    const enabledIds = Array.from(selectedIds).filter(id => {
      const cred = data?.credentials.find(c => c.id === id)
      return cred && !cred.disabled
    })

    if (enabledIds.length === 0) {
      toast.error('选中的凭据中没有启用的凭据')
      return
    }

    setBatchQueryingBalance(true)
    setBatchQueryBalanceProgress({ current: 0, total: enabledIds.length })

    try {
      const resp = await refreshBalancesBatch(enabledIds)
      setBatchQueryBalanceProgress({ current: enabledIds.length, total: enabledIds.length })

      // 成功项合入 balanceMap（复用单凭证查询的展示链路）
      setBalanceMap(prev => {
        const next = new Map(prev)
        resp.results.forEach(item => {
          if (item.success && item.balance) {
            next.set(item.id, item.balance)
          }
        })
        return next
      })

      if (resp.failureCount === 0) {
        toast.success(`成功查询 ${resp.successCount} 个凭据的余额`)
      } else {
        toast.warning(`查询余额：成功 ${resp.successCount} 个，失败 ${resp.failureCount} 个`)
      }
    } catch (error) {
      toast.error(`批量查询余额失败：${extractErrorMessage(error)}`)
    } finally {
      setBatchQueryingBalance(false)
    }

    deselectAll()
  }

  // 一键清除所有已禁用凭据
  const handleClearAll = async () => {
    if (!data?.credentials || data.credentials.length === 0) {
      toast.error('没有可清除的凭据')
      return
    }

    const disabledCredentials = data.credentials.filter(credential => credential.disabled)

    if (disabledCredentials.length === 0) {
      toast.error('没有可清除的已禁用凭据')
      return
    }

    if (!confirm(`确定要清除所有 ${disabledCredentials.length} 个已禁用凭据吗？此操作无法撤销。`)) {
      return
    }

    let successCount = 0
    let failCount = 0

    for (const credential of disabledCredentials) {
      try {
        await new Promise<void>((resolve, reject) => {
          deleteCredential(credential.id, {
            onSuccess: () => {
              successCount++
              resolve()
            },
            onError: (err) => {
              failCount++
              reject(err)
            }
          })
        })
      } catch (error) {
        // 错误已在 onError 中处理
      }
    }

    if (failCount === 0) {
      toast.success(`成功清除所有 ${successCount} 个已禁用凭据`)
    } else {
      toast.warning(`清除已禁用凭据：成功 ${successCount} 个，失败 ${failCount} 个`)
    }

    deselectAll()
  }

  // 查询所有未禁用凭据信息（一次往返调 batch 端点，不刷 token）
  const handleQueryCurrentPageInfo = async () => {
    if (!data?.credentials || data.credentials.length === 0) {
      toast.error('暂无可查询的凭据')
      return
    }

    const ids = data.credentials
      .filter(credential => !credential.disabled)
      .map(credential => credential.id)

    if (ids.length === 0) {
      toast.error('没有可查询的启用凭据')
      return
    }

    setQueryingInfo(true)
    setQueryInfoProgress({ current: 0, total: ids.length })
    setLoadingBalanceIds(prev => {
      const next = new Set(prev)
      ids.forEach(id => next.add(id))
      return next
    })

    try {
      const resp = await refreshBalancesBatch(ids)
      setQueryInfoProgress({ current: ids.length, total: ids.length })

      setBalanceMap(prev => {
        const next = new Map(prev)
        resp.results.forEach(item => {
          if (item.success && item.balance) {
            next.set(item.id, item.balance)
          }
        })
        return next
      })

      if (resp.failureCount === 0) {
        toast.success(`查询完成：成功 ${resp.successCount}/${ids.length}`)
      } else {
        toast.warning(`查询完成：成功 ${resp.successCount} 个，失败 ${resp.failureCount} 个`)
      }
    } catch (error) {
      toast.error(`批量查询失败：${extractErrorMessage(error)}`)
    } finally {
      setLoadingBalanceIds(prev => {
        const next = new Set(prev)
        ids.forEach(id => next.delete(id))
        return next
      })
      setQueryingInfo(false)
    }
  }

  // 批量导出（token.json 兼容格式）
  const handleBatchExport = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要导出的凭据')
      return
    }
    try {
      const ids = Array.from(selectedIds)
      const items = await exportTokenJson(ids)
      if (items.length === 0) {
        toast.warning('未导出任何凭据（API Key 凭据不支持导出）')
        return
      }
      const json = JSON.stringify(items, null, 2)
      const blob = new Blob([json], { type: 'application/json' })
      const url = URL.createObjectURL(blob)
      const ts = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19)
      const a = document.createElement('a')
      a.href = url
      a.download = `kiro-tokens-${ts}.json`
      document.body.appendChild(a)
      a.click()
      document.body.removeChild(a)
      URL.revokeObjectURL(url)
      const skipped = ids.length - items.length
      toast.success(
        skipped > 0
          ? `已导出 ${items.length} 项，跳过 ${skipped} 项（API Key / 缺 refreshToken）`
          : `已导出 ${items.length} 项`
      )
    } catch (error) {
      toast.error(`导出失败：${extractErrorMessage(error)}`)
    }
  }

  // KAM 兼容导出（kiro-account-manager 可直接 import）
  const handleBatchExportKam = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要导出的凭据')
      return
    }
    try {
      const ids = Array.from(selectedIds)
      const items = await exportKam(ids)
      if (items.length === 0) {
        toast.warning('未导出任何凭据（API Key / 缺 refreshToken）')
        return
      }
      const json = JSON.stringify(items, null, 2)
      const blob = new Blob([json], { type: 'application/json' })
      const url = URL.createObjectURL(blob)
      const today = new Date().toISOString().slice(0, 10)
      const a = document.createElement('a')
      a.href = url
      a.download = `kiro-accounts-${items.length}-${today}.json`
      document.body.appendChild(a)
      a.click()
      document.body.removeChild(a)
      URL.revokeObjectURL(url)
      const skipped = ids.length - items.length
      toast.success(
        skipped > 0
          ? `已导出 ${items.length} 项 (KAM)，跳过 ${skipped} 项`
          : `已导出 ${items.length} 项 (KAM)`
      )
    } catch (error) {
      toast.error(`导出失败：${extractErrorMessage(error)}`)
    }
  }

  // 批量验活
  const handleBatchVerify = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要验活的凭据')
      return
    }

    // 初始化状态
    setVerifying(true)
    cancelVerifyRef.current = false
    const ids = Array.from(selectedIds)
    setVerifyProgress({ current: 0, total: ids.length })

    let successCount = 0

    // 初始化结果，所有凭据状态为 pending
    const initialResults = new Map<number, VerifyResult>()
    ids.forEach(id => {
      initialResults.set(id, { id, status: 'pending' })
    })
    setVerifyResults(initialResults)
    setVerifyDialogOpen(true)

    // 开始验活
    for (let i = 0; i < ids.length; i++) {
      // 检查是否取消
      if (cancelVerifyRef.current) {
        toast.info('已取消验活')
        break
      }

      const id = ids[i]

      // 更新当前凭据状态为 verifying
      setVerifyResults(prev => {
        const newResults = new Map(prev)
        newResults.set(id, { id, status: 'verifying' })
        return newResults
      })

      try {
        const balance = await getCredentialBalance(id)
        successCount++

        // 更新为成功状态
        setVerifyResults(prev => {
          const newResults = new Map(prev)
          newResults.set(id, {
            id,
            status: 'success',
            usage: `${balance.currentUsage}/${balance.usageLimit}`
          })
          return newResults
        })
      } catch (error) {
        // 更新为失败状态
        setVerifyResults(prev => {
          const newResults = new Map(prev)
          newResults.set(id, {
            id,
            status: 'failed',
            error: extractErrorMessage(error)
          })
          return newResults
        })
      }

      // 更新进度
      setVerifyProgress({ current: i + 1, total: ids.length })

      // 添加延迟防止封号（最后一个不需要延迟）
      if (i < ids.length - 1 && !cancelVerifyRef.current) {
        await new Promise(resolve => setTimeout(resolve, 2000))
      }
    }

    setVerifying(false)

    if (!cancelVerifyRef.current) {
      toast.success(`验活完成：成功 ${successCount}/${ids.length}`)
    }
  }

  // 取消验活
  const handleCancelVerify = () => {
    cancelVerifyRef.current = true
    setVerifying(false)
  }

  if (isLoading) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-background">
        <div className="text-center">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-primary mx-auto mb-4"></div>
          <p className="text-muted-foreground">加载中...</p>
        </div>
      </div>
    )
  }

  if (error) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-background p-4">
        <Card className="w-full max-w-md">
          <CardContent className="pt-6 text-center">
            <div className="text-red-500 mb-4">加载失败</div>
            <p className="text-muted-foreground mb-4">{(error as Error).message}</p>
            <div className="space-x-2">
              <Button onClick={() => refetch()}>重试</Button>
              <Button variant="outline" onClick={handleLogout}>重新登录</Button>
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="min-h-screen bg-background">
      {/* 顶部导航 */}
      <header className="sticky top-0 z-50 w-full border-b bg-background/80 backdrop-blur supports-[backdrop-filter]:bg-background/60">
        <div className="mx-auto flex h-12 w-full max-w-[2400px] items-center justify-between px-4 sm:px-6 lg:px-8 2xl:px-10">
          <div className="flex items-center gap-2.5">
            <div className="flex h-7 w-7 items-center justify-center rounded-md bg-primary text-primary-foreground">
              <Server className="h-4 w-4" />
            </div>
            <span className="text-sm font-semibold tracking-tight">Kiro Admin</span>
            <div className="ml-3 hidden items-center gap-3 text-xs text-muted-foreground sm:flex">
              <span className="tabular">
                <span className="font-medium text-foreground">{data?.total ?? 0}</span> 总数
              </span>
              <span className="h-3 w-px bg-border" />
              <span className="tabular">
                <span className="font-medium text-success">{data?.available ?? 0}</span> 可用
              </span>
              {disabledCredentialCount > 0 && (
                <>
                  <span className="h-3 w-px bg-border" />
                  <span className="tabular">
                    <span className="font-medium text-muted-foreground">{disabledCredentialCount}</span> 禁用
                  </span>
                </>
              )}
            </div>
          </div>
          <div className="flex items-center gap-1">
            <Button
              variant="ghost"
              size="icon"
              className="h-8 w-auto px-2 gap-1"
              onClick={() => {
                const i = uiScales.indexOf(uiScale)
                setUiScale(uiScales[(i + 1) % uiScales.length])
              }}
              title={`UI 缩放 ${uiScale}%（点击循环 ${uiScales.join(' / ')}%）`}
            >
              <ZoomIn className="h-4 w-4" />
              <span className="text-xs tabular text-muted-foreground">{uiScale}%</span>
            </Button>
            <Button variant="ghost" size="icon" className="h-8 w-8" onClick={toggleDarkMode} title="切换主题">
              {darkMode ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
            </Button>
            <Button variant="ghost" size="icon" className="h-8 w-8" onClick={() => setSystemPromptDialogOpen(true)} title="系统提示">
              <FileText className="h-4 w-4" />
            </Button>
            <Button variant="ghost" size="icon" className="h-8 w-8" onClick={() => setProxyPoolDialogOpen(true)} title="代理池">
              <Network className="h-4 w-4" />
            </Button>
            <Button variant="ghost" size="icon" className="h-8 w-8" onClick={() => setSettingsDialogOpen(true)} title="设置">
              <Settings className="h-4 w-4" />
            </Button>
            <Button variant="ghost" size="icon" className="h-8 w-8" onClick={handleRefresh} title="刷新">
              <RefreshCw className="h-4 w-4" />
            </Button>
            <div className="mx-1 h-4 w-px bg-border" />
            <Button variant="ghost" size="icon" className="h-8 w-8" onClick={handleLogout} title="退出登录">
              <LogOut className="h-4 w-4" />
            </Button>
          </div>
        </div>
      </header>

      {/* 主内容 */}
      <main className="mx-auto w-full max-w-[2400px] px-4 sm:px-6 lg:px-8 2xl:px-10 py-6">
        {/* 工具栏：选择/批量/添加 */}
        <div className="mb-5 flex flex-col gap-3 lg:flex-row lg:items-center lg:justify-between">
          <div className="flex items-center gap-3">
            <h2 className="text-lg font-semibold tracking-tight">凭据管理</h2>
            {selectedIds.size > 0 && (
              <div className="flex items-center gap-2">
                <Badge variant="secondary" className="rounded-full px-2.5 py-0.5 text-xs">
                  已选 {selectedIds.size}
                </Badge>
                <Button onClick={deselectAll} size="sm" variant="ghost" className="h-7 px-2 text-xs">
                  取消选择
                </Button>
              </div>
            )}
          </div>
          <div className="flex flex-wrap gap-2">
            {selectedIds.size > 0 && (
              <>
                <Button onClick={handleBatchVerify} size="sm" variant="outline" className="h-8">
                  <CheckCircle2 className="h-3.5 w-3.5 mr-1.5" />
                  批量验活
                </Button>
                <Button
                  onClick={handleBatchForceRefresh}
                  size="sm"
                  variant="outline"
                  className="h-8"
                  disabled={batchRefreshing}
                >
                  <RefreshCw className={`h-3.5 w-3.5 mr-1.5 ${batchRefreshing ? 'animate-spin' : ''}`} />
                  {batchRefreshing ? `刷新中 ${batchRefreshProgress.current}/${batchRefreshProgress.total}` : '批量刷新'}
                </Button>
                <Button
                  onClick={handleBatchQueryBalance}
                  size="sm"
                  variant="outline"
                  className="h-8"
                  disabled={batchQueryingBalance}
                >
                  <RefreshCw className={`h-3.5 w-3.5 mr-1.5 ${batchQueryingBalance ? 'animate-spin' : ''}`} />
                  {batchQueryingBalance ? `查询 ${batchQueryBalanceProgress.current}/${batchQueryBalanceProgress.total}` : '批量查询'}
                </Button>
                <Button onClick={handleBatchResetFailure} size="sm" variant="outline" className="h-8">
                  <RotateCcw className="h-3.5 w-3.5 mr-1.5" />
                  恢复异常
                </Button>
                <Button onClick={handleBatchExport} size="sm" variant="outline" className="h-8">
                  <Download className="h-3.5 w-3.5 mr-1.5" />
                  批量导出
                </Button>
                <Button onClick={handleBatchExportKam} size="sm" variant="outline" className="h-8" title="导出为 kiro-account-manager 兼容格式">
                  <Download className="h-3.5 w-3.5 mr-1.5" />
                  导出KAM
                </Button>
                <Button
                  onClick={handleBatchDelete}
                  size="sm"
                  variant="destructive"
                  className="h-8"
                  disabled={selectedIds.size === 0}
                  title={`删除选中的 ${selectedIds.size} 个凭据（任意状态）`}
                >
                  <Trash2 className="h-3.5 w-3.5 mr-1.5" />
                  批量删除
                </Button>
                <Button
                  onClick={() => handleBatchDisable(true)}
                  size="sm"
                  variant="outline"
                  className="h-8"
                  disabled={selectedIds.size === 0}
                  title={`禁用选中的 ${selectedIds.size} 个凭据`}
                >
                  批量禁用
                </Button>
                <Button
                  onClick={() => handleBatchDisable(false)}
                  size="sm"
                  variant="outline"
                  className="h-8"
                  disabled={selectedIds.size === 0}
                  title={`启用选中的 ${selectedIds.size} 个凭据`}
                >
                  批量启用
                </Button>
                <span className="mx-1 h-6 w-px self-center bg-border" />
              </>
            )}
            {verifying && !verifyDialogOpen && (
              <Button onClick={() => setVerifyDialogOpen(true)} size="sm" variant="secondary" className="h-8">
                <CheckCircle2 className="h-3.5 w-3.5 mr-1.5 animate-spin" />
                验活中 {verifyProgress.current}/{verifyProgress.total}
              </Button>
            )}
            {data?.credentials && data.credentials.length > 0 && (
              <Button
                onClick={handleQueryCurrentPageInfo}
                size="sm"
                variant="outline"
                className="h-8"
                disabled={queryingInfo}
              >
                <RefreshCw className={`h-3.5 w-3.5 mr-1.5 ${queryingInfo ? 'animate-spin' : ''}`} />
                {queryingInfo ? `查询 ${queryInfoProgress.current}/${queryInfoProgress.total}` : '查询本页'}
              </Button>
            )}
            {data?.credentials && data.credentials.length > 0 && (
              <Button
                onClick={handleClearAll}
                size="sm"
                variant="outline"
                className="h-8 text-destructive hover:text-destructive"
                disabled={disabledCredentialCount === 0}
                title={disabledCredentialCount === 0 ? '没有可清除的已禁用凭据' : undefined}
              >
                <Trash2 className="h-3.5 w-3.5 mr-1.5" />
                清除已禁用
              </Button>
            )}
            <Button onClick={() => setKamImportDialogOpen(true)} size="sm" variant="outline" className="h-8">
              <FileUp className="h-3.5 w-3.5 mr-1.5" />
              KAM 导入
            </Button>
            <Button onClick={() => setBatchImportDialogOpen(true)} size="sm" variant="outline" className="h-8">
              <Upload className="h-3.5 w-3.5 mr-1.5" />
              批量导入
            </Button>
            <Button
              variant={compactMode ? 'default' : 'outline'}
              size="sm"
              className="h-8"
              onClick={() => {
                const next = !compactMode
                setCompactMode(next)
                try { localStorage.setItem('kiro-compact-mode', next ? '1' : '0') } catch {}
              }}
              title={compactMode ? '切换到详细视图' : '切换到紧凑视图'}
            >
              {compactMode ? '详细' : '紧凑'}
            </Button>
            <Button onClick={() => setAddDialogOpen(true)} size="sm" className="h-8">
              <Plus className="h-3.5 w-3.5 mr-1.5" />
              添加凭据
            </Button>
          </div>
        </div>

        {/* 凭据列表 */}
        {/* 分组 / 渠道快速过滤卡片 */}
        {(allGroups.length > 0 || allSources.length > 0) && (
          <div className="mb-3 flex flex-wrap gap-2 items-center">
            {allSources.length > 0 && (
              <div className="flex flex-wrap gap-1.5 items-center">
                <span className="text-xs text-muted-foreground mr-1">渠道:</span>
                <button
                  onClick={() => { setFilterSource(''); setCurrentPage(1) }}
                  className={`h-7 rounded-full px-3 text-xs font-medium border transition-colors ${filterSource === '' ? 'bg-primary text-primary-foreground border-primary' : 'bg-background border-border hover:bg-muted'}`}
                >
                  全部 ({allCredentials.length})
                </button>
                {allSources.map(src => {
                  const cnt = allCredentials.filter(c => c.source === src).length
                  const activeCnt = allCredentials.filter(c => c.source === src && !c.disabled).length
                  return (
                    <button
                      key={src}
                      onClick={() => { setFilterSource(src); setCurrentPage(1) }}
                      className={`h-7 rounded-full px-3 text-xs font-medium border transition-colors ${filterSource === src ? 'bg-purple-500 text-white border-purple-500' : 'bg-background border-purple-300 hover:bg-purple-50 dark:hover:bg-purple-950 text-purple-700 dark:text-purple-400'}`}
                      title={`存活: ${activeCnt}/${cnt}`}
                    >
                      {src} ({activeCnt}/{cnt})
                    </button>
                  )
                })}
              </div>
            )}
            {allGroups.length > 0 && (
              <div className="flex flex-wrap gap-1.5 items-center">
                <span className="text-xs text-muted-foreground mr-1">分组:</span>
                <button
                  onClick={() => { setFilterGroup(''); setCurrentPage(1) }}
                  className={`h-7 rounded-full px-3 text-xs font-medium border transition-colors ${filterGroup === '' ? 'bg-primary text-primary-foreground border-primary' : 'bg-background border-border hover:bg-muted'}`}
                >
                  全部
                </button>
                {allGroups.map(grp => {
                  const cnt = allCredentials.filter(c => c.group === grp).length
                  const activeCnt = allCredentials.filter(c => c.group === grp && !c.disabled).length
                  return (
                    <button
                      key={grp}
                      onClick={() => { setFilterGroup(grp); setCurrentPage(1) }}
                      className={`h-7 rounded-full px-3 text-xs font-medium border transition-colors ${filterGroup === grp ? 'bg-blue-500 text-white border-blue-500' : 'bg-background border-blue-300 hover:bg-blue-50 dark:hover:bg-blue-950 text-blue-700 dark:text-blue-400'}`}
                      title={`存活: ${activeCnt}/${cnt}`}
                    >
                      {grp} ({activeCnt}/{cnt})
                    </button>
                  )
                })}
              </div>
            )}
          </div>
        )}

        {data?.credentials.length === 0 ? (
          <Card>
            <CardContent className="py-16 text-center text-sm text-muted-foreground">
              暂无凭据，点击「添加凭据」开始
            </CardContent>
          </Card>
        ) : (
          <>
            {compactMode ? (
              /* 紧凑视图：KAM 风格表格 — 邮箱/来源/订阅/配额(主+超额双条)/状态/过期/分组 */
              <div className="rounded-lg border border-border overflow-hidden">
                {/* 表头 */}
                <div className="flex items-center gap-2 px-3 py-2 bg-muted/50 text-xs font-medium text-muted-foreground border-b border-border">
                  <span className="w-4 shrink-0" />
                  <span className="w-44 shrink-0">邮箱</span>
                  <span className="w-20 shrink-0">来源</span>
                  <span className="w-24 shrink-0">订阅</span>
                  <span className="flex-1 min-w-[220px]">配额 / 超额</span>
                  <span className="w-16 shrink-0 text-center">状态</span>
                  <span className="w-28 shrink-0">配额重置</span>
                  <span className="w-20 shrink-0">分组</span>
                </div>
                {/* 行 */}
                <div className="divide-y divide-border">
                  {currentCredentials.map((credential) => {
                    const bal = balanceMap.get(credential.id) || null
                    const limit = bal?.usageLimit ?? 0
                    const used = bal?.currentUsage ?? 0
                    const baseRemaining = Math.max(0, limit - used)
                    const basePercent = limit > 0 ? Math.min(100, (used / limit) * 100) : 0
                    const overCap = bal?.overageCap ?? 0
                    const overUsed = Math.max(0, used - limit)
                    const overRemaining = Math.max(0, overCap - overUsed)
                    const overPercent = overCap > 0 ? Math.min(100, (overUsed / overCap) * 100) : 0
                    const overageOn = bal?.overageStatus === 'ENABLED'
                    const disabled = credential.disabled
                    const label = credential.email || `#${credential.id}`
                    const sub = bal?.subscriptionTitle || null
                    return (
                      <div
                        key={credential.id}
                        className={`flex items-center gap-2 px-3 py-2 text-xs transition-colors ${disabled ? 'opacity-50' : ''} ${selectedIds.has(credential.id) ? 'bg-primary/5' : 'hover:bg-muted/30'}`}
                      >
                        <input
                          type="checkbox"
                          checked={selectedIds.has(credential.id)}
                          onChange={() => toggleSelect(credential.id)}
                          className="h-3.5 w-3.5 shrink-0 cursor-pointer"
                        />
                        {/* 邮箱 + 标签 */}
                        <div className="w-44 shrink-0 min-w-0">
                          <div className="truncate font-mono text-foreground" title={credential.email || String(credential.id)}>{label}</div>
                        </div>
                        {/* 来源 */}
                        <div className="w-20 shrink-0">
                          {credential.source
                            ? <span className="inline-block px-1.5 py-0.5 rounded bg-secondary text-muted-foreground truncate max-w-full" title={credential.source}>{credential.source}</span>
                            : <span className="text-muted-foreground/40">—</span>}
                        </div>
                        {/* 订阅 */}
                        <div className="w-24 shrink-0">
                          {sub
                            ? <span className="inline-block px-1.5 py-0.5 rounded bg-blue-500/15 text-blue-500 font-medium truncate max-w-full" title={sub}>{sub}</span>
                            : <span className="text-muted-foreground/40">—</span>}
                        </div>
                        {/* 配额：主条 + 超额条 并排 */}
                        <div className="flex-1 min-w-[220px] flex items-center gap-5">
                          {/* 主配额 */}
                          <div className="flex-1 flex items-center gap-2">
                            <span className="w-20 shrink-0 text-right tabular-nums text-muted-foreground">
                              {limit > 0 ? <><span className="text-foreground font-medium">{Math.round(baseRemaining)}</span>/{limit}</> : '—'}
                            </span>
                            <div className="flex-1 h-1.5 rounded-full bg-secondary overflow-hidden">
                              <div
                                className={`h-full rounded-full transition-all ${basePercent >= 90 ? 'bg-destructive' : basePercent >= 70 ? 'bg-yellow-500' : 'bg-green-500'}`}
                                style={{ width: `${basePercent}%` }}
                              />
                            </div>
                          </div>
                          {/* 超额 */}
                          {(overCap > 0 || overageOn) ? (
                            <div className="flex-1 flex items-center gap-2">
                              <span className="w-24 shrink-0 text-right tabular-nums text-yellow-600 dark:text-yellow-500 flex items-center justify-end gap-0.5">
                                ⚡{overCap > 0 ? <><span className="font-medium">{Math.round(overRemaining)}</span>/{overCap}</> : '已开'}
                              </span>
                              <div className="flex-1 h-1.5 rounded-full bg-secondary overflow-hidden">
                                <div
                                  className={`h-full rounded-full transition-all ${overPercent >= 90 ? 'bg-destructive' : 'bg-yellow-500'}`}
                                  style={{ width: `${overPercent}%` }}
                                />
                              </div>
                            </div>
                          ) : (
                            <div className="flex-1 text-muted-foreground/30 text-center">—</div>
                          )}
                        </div>
                        {/* 状态 */}
                        <div className="w-16 shrink-0 text-center">
                          {disabled
                            ? <span className="inline-block px-1.5 py-0.5 rounded bg-destructive/15 text-destructive">禁用</span>
                            : <span className="inline-block px-1.5 py-0.5 rounded bg-green-500/15 text-green-600 dark:text-green-500">正常</span>}
                        </div>
                        {/* 配额重置时间（来自余额 nextResetAt，非 token 刷新时间） */}
                        <div className="w-28 shrink-0 tabular-nums text-muted-foreground truncate" title={bal?.nextResetAt ? new Date(bal.nextResetAt * 1000).toLocaleString('zh-CN') : ''}>
                          {bal?.nextResetAt
                            ? new Date(bal.nextResetAt * 1000).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' })
                            : '—'}
                        </div>
                        {/* 分组 */}
                        <div className="w-20 shrink-0">
                          {credential.group
                            ? <span className="inline-block px-1.5 py-0.5 rounded bg-purple-500/15 text-purple-500 truncate max-w-full" title={credential.group}>{credential.group}</span>
                            : <span className="text-muted-foreground/40">—</span>}
                        </div>
                      </div>
                    )
                  })}
                </div>
              </div>
            ) : (
              /* 详细视图：原卡片网格 */
              <div className="grid gap-4 grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 2xl:grid-cols-4 3xl:grid-cols-5 4xl:grid-cols-6">
                {currentCredentials.map((credential) => (
                  <CredentialCard
                    key={credential.id}
                    credential={credential}
                    onViewBalance={handleViewBalance}
                    onViewModels={handleViewModels}
                    selected={selectedIds.has(credential.id)}
                    onToggleSelect={() => toggleSelect(credential.id)}
                    balance={balanceMap.get(credential.id) || null}
                    loadingBalance={loadingBalanceIds.has(credential.id)}
                    onBalanceChange={(id, next) => {
                      setBalanceMap(prev => {
                        const m = new Map(prev)
                        if (next) m.set(id, next)
                        else m.delete(id)
                        return m
                      })
                    }}
                  />
                ))}
              </div>
            )}

            {/* 分页控件 + 每页条数 */}
            {filteredCredentials.length > 0 && (
              <div className="mt-6 flex items-center justify-center gap-3 text-sm flex-wrap">
                <Button
                  variant="outline"
                  size="sm"
                  className="h-8"
                  onClick={() => setCurrentPage(p => Math.max(1, p - 1))}
                  disabled={currentPage === 1}
                >
                  上一页
                </Button>
                <span className="tabular text-muted-foreground">
                  {currentPage} / {Math.max(1, totalPages)} · 共 {filteredCredentials.length} 个
                </span>
                <Button
                  variant="outline"
                  size="sm"
                  className="h-8"
                  onClick={() => setCurrentPage(p => Math.min(totalPages, p + 1))}
                  disabled={currentPage >= totalPages}
                >
                  下一页
                </Button>
                <div className="flex items-center gap-1.5 ml-2">
                  <span className="text-muted-foreground">每页</span>
                  <select
                    value={itemsPerPage}
                    onChange={(e) => handlePageSizeChange(Number(e.target.value))}
                    className="h-8 rounded-md border border-border bg-background px-2 text-sm cursor-pointer focus:outline-none focus:ring-1 focus:ring-ring"
                  >
                    {[12, 24, 48, 96, 200].map(n => (
                      <option key={n} value={n}>{n}</option>
                    ))}
                  </select>
                  <span className="text-muted-foreground">条</span>
                </div>
              </div>
            )}
          </>
        )}
      </main>

      {/* 余额对话框 */}
      <BalanceDialog
        credentialId={selectedCredentialId}
        open={balanceDialogOpen}
        onOpenChange={setBalanceDialogOpen}
      />

      {/* 模型列表对话框 */}
      <ModelsDialog
        credentialId={selectedCredentialId}
        open={modelsDialogOpen}
        onOpenChange={setModelsDialogOpen}
      />

      {/* 添加凭据对话框 */}
      <AddCredentialDialog
        open={addDialogOpen}
        onOpenChange={setAddDialogOpen}
      />

      {/* 批量导入对话框 */}
      <BatchImportDialog
        open={batchImportDialogOpen}
        onOpenChange={setBatchImportDialogOpen}
      />

      {/* KAM 账号导入对话框 */}
      <KamImportDialog
        open={kamImportDialogOpen}
        onOpenChange={setKamImportDialogOpen}
        onJobStart={(jobId, total) => setActiveImportJob({ jobId, total })}
      />

      {/* KAM 后台导入进度浮窗 */}
      {activeImportJob && (
        <ImportJobToast
          jobId={activeImportJob.jobId}
          total={activeImportJob.total}
          onDone={() => setActiveImportJob(null)}
        />
      )}

      {/* 批量验活对话框 */}
      <BatchVerifyDialog
        open={verifyDialogOpen}
        onOpenChange={setVerifyDialogOpen}
        verifying={verifying}
        progress={verifyProgress}
        results={verifyResults}
        onCancel={handleCancelVerify}
      />

      {/* 设置对话框 */}
      <SettingsDialog
        open={settingsDialogOpen}
        onOpenChange={setSettingsDialogOpen}
      />

      {/* 系统提示对话框 */}
      <SystemPromptDialog
        open={systemPromptDialogOpen}
        onOpenChange={setSystemPromptDialogOpen}
      />

      {/* 代理池对话框 */}
      <ProxyPoolDialog
        open={proxyPoolDialogOpen}
        onOpenChange={setProxyPoolDialogOpen}
      />
    </div>
  )
}
