@include('partials.header')
@include("partials.footer")

<livewire:user-profile />
<livewire:nav.main-menu />

<button wire:click="save">Save</button>
<button wire:click='refresh'>Refresh</button>
