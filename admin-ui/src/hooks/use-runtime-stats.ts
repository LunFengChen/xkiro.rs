import { useQuery } from '@tanstack/react-query'
import { getRuntimeStats } from '@/api/credentials'
import type { RuntimeStatsItem, RuntimeStatsResponse } from '@/types/api'

/**
 * 高频轮询凭据运行时状态（K/N、lastUsedAt）。
 *
 * 设计动机：
 * - 凭据列表 `useCredentials`（30s 轮询）拉的是完整结构，包含数据库字段、上下游配置等重数据；
 * - K/N 信号量、lastUsedAt 这类只在内存里变动的字段需要更高的实时性；
 * - 拆出独立轻量端点 `/credentials/runtime-stats` + 1.5s 轮询，避免高频拉重 payload。
 *
 * 返回 `Map<id, RuntimeStatsItem>` 方便 dashboard 在渲染时按 id O(1) merge 到 credentials 数组。
 */
export function useRuntimeStats() {
  return useQuery<RuntimeStatsResponse, Error, Map<number, RuntimeStatsItem>>({
    queryKey: ['credentials', 'runtime-stats'],
    queryFn: getRuntimeStats,
    refetchInterval: 1500,
    // 切到后台标签时停止轮询，回到前台立刻刷新
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
    // 转成 Map 便于 dashboard 按 id 查找
    select: (data) => {
      const map = new Map<number, RuntimeStatsItem>()
      for (const item of data.credentials) {
        map.set(item.id, item)
      }
      return map
    },
    // 静默错误：运行时状态拉取失败不应该打断主列表渲染
    retry: 1,
  })
}
