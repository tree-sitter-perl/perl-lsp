<?php
namespace App\Wp;

class Plugin
{
    public function register(): void
    {
        add_action('init', [$this, 'onInit']);
        add_filter('the_title', [$this, 'filterTitle'], 10, 2);
    }

    public function onInit(): void
    {
    }

    public function filterTitle(string $title, int $id): string
    {
        return $title;
    }
}
