<script setup lang="ts">
import axios from 'axios'
import { formatName } from './helper'

interface Props {
  title: string
}

const count = 0

function greet(name: string): string {
  return formatName(name)
}

function bump(): void {
  greet('tick')
  axios.get('/refresh')
}
</script>

<template>
  <button @click="() => import('./LazyWidget.vue')">
    {{ greet(title) }} {{ count }} {{ bump }}
  </button>
</template>

<style scoped>
button { color: red; }
</style>
