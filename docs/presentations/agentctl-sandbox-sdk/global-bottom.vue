<script setup lang="ts">
import { useNav } from '@slidev/client'
const { currentPage, total, currentLayout } = useNav()
</script>

<template>
  <footer
    v-if="currentLayout !== 'cover' && currentLayout !== 'end'"
    class="absolute bottom-0 left-0 right-0 px-10 py-3 flex items-center justify-between text-xs"
    style="color: var(--dd-muted)"
  >
    <span class="flex items-center gap-2">
      <span class="accent-bar" style="width: 36px; height: 3px"><span /><span /><span /><span /></span>
      agentctl · Sandbox SDK
    </span>
    <span>{{ currentPage }} / {{ total }}</span>
  </footer>
</template>
