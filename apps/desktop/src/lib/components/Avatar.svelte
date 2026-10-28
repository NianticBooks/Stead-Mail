<script lang="ts">
  import { initials } from "../format";

  let { label, size = 32 }: { label: string; size?: number } = $props();

  // A stable colour per sender, from a hash of the label.
  const hue = $derived([...label].reduce((h, c) => (h * 31 + c.charCodeAt(0)) % 360, 7));
</script>

<span
  class="avatar"
  style:width="{size}px"
  style:height="{size}px"
  style:font-size="{Math.round(size * 0.38)}px"
  style:background="hsl({hue} 38% 26%)"
  style:color="hsl({hue} 70% 82%)"
  aria-hidden="true">{initials(label)}</span
>

<style>
  .avatar {
    display: inline-grid;
    place-items: center;
    border-radius: 50%;
    font-weight: 600;
    flex: none;
    letter-spacing: 0.02em;
  }
</style>
