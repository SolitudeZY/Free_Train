<script lang="ts">
  import { ArrowLeft, Gauge, Image as ImageIcon } from "lucide-svelte";
  import { onMount } from "svelte";

  const itemCount = 100_000;
  const rowHeight = 112;
  const minimumColumnWidth = 148;
  const overscanRows = 4;

  let viewport: HTMLDivElement;
  let scrollTop = $state(0);
  let viewportHeight = $state(720);
  let viewportWidth = $state(1200);

  const columns = $derived(Math.max(1, Math.floor(viewportWidth / minimumColumnWidth)));
  const totalRows = $derived(Math.ceil(itemCount / columns));
  const startRow = $derived(Math.max(0, Math.floor(scrollTop / rowHeight) - overscanRows));
  const endRow = $derived(
    Math.min(totalRows, Math.ceil((scrollTop + viewportHeight) / rowHeight) + overscanRows),
  );
  const startIndex = $derived(startRow * columns);
  const endIndex = $derived(Math.min(itemCount, endRow * columns));
  const visibleItems = $derived(
    Array.from({ length: endIndex - startIndex }, (_, offset) => startIndex + offset),
  );

  onMount(() => {
    const observer = new ResizeObserver(([entry]) => {
      viewportHeight = entry.contentRect.height;
      viewportWidth = entry.contentRect.width;
    });
    observer.observe(viewport);
    return () => observer.disconnect();
  });
</script>

<svelte:head><title>M0 虚拟网格探针</title></svelte:head>

<main class="probe-shell">
  <header>
    <a href="/" aria-label="返回工作台"><ArrowLeft size={17} /></a>
    <div><span>M0 技术探针</span><h1>100,000 项虚拟缩略图网格</h1></div>
    <div class="metrics">
      <span><Gauge size={14} />当前 DOM 项</span>
      <strong>{visibleItems.length}</strong>
      <span>总项数</span>
      <strong>{itemCount.toLocaleString()}</strong>
    </div>
  </header>

  <div class="viewport" bind:this={viewport} onscroll={(event) => (scrollTop = event.currentTarget.scrollTop)}>
    <div class="spacer" style:height={`${totalRows * rowHeight}px`}>
      <div
        class="visible-grid"
        style:transform={`translateY(${startRow * rowHeight}px)`}
        style:grid-template-columns={`repeat(${columns}, minmax(0, 1fr))`}
      >
        {#each visibleItems as index}
          <article>
            <div class="thumb"><ImageIcon size={20} /><span>{String(index + 1).padStart(6, "0")}</span></div>
            <strong>候选图片 {index + 1}</strong>
            <small>cam_{String((index % 12) + 1).padStart(2, "0")}</small>
          </article>
        {/each}
      </div>
    </div>
  </div>
</main>

<style>
  :global(html), :global(body) { overflow: hidden; }
  .probe-shell { display: grid; grid-template-rows: 58px minmax(0, 1fr); width: 100vw; height: 100vh; background: var(--bg-app); }
  header { display: flex; align-items: center; gap: 12px; padding: 0 16px; background: var(--bg-panel-strong); border-bottom: 1px solid var(--border); }
  header a { display: grid; place-items: center; width: 30px; height: 30px; color: var(--text); border: 1px solid var(--border); border-radius: 5px; }
  header span, header h1 { display: block; margin: 0; }
  header span { color: var(--text-faint); font-size: 9px; }
  header h1 { margin-top: 2px; font-size: 14px; }
  .metrics { display: grid; grid-template-columns: auto auto auto auto; align-items: center; gap: 7px 12px; margin-left: auto; font-size: 10px; }
  .metrics span { display: flex; align-items: center; gap: 5px; color: var(--text-muted); }
  .metrics strong { font-family: var(--font-mono); font-size: 12px; }
  .viewport { min-height: 0; overflow: auto; }
  .spacer { position: relative; width: 100%; }
  .visible-grid { position: absolute; top: 0; right: 0; left: 0; display: grid; gap: 8px; padding: 8px; }
  article { min-width: 0; height: 104px; padding: 6px; background: var(--bg-panel); border: 1px solid var(--border); border-radius: 5px; }
  .thumb { position: relative; display: grid; place-items: center; height: 68px; color: var(--text-faint); background: var(--bg-hover); border: 1px solid var(--border); }
  .thumb span { position: absolute; right: 4px; bottom: 3px; color: var(--text-muted); font-family: var(--font-mono); font-size: 8px; }
  article strong, article small { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  article strong { margin-top: 5px; font-size: 9px; }
  article small { margin-top: 1px; color: var(--text-faint); font-size: 8px; }
</style>
