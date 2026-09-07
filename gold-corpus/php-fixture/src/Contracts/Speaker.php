<?php
namespace App\Contracts;

interface Speaker
{
    public function speak(string $line): string;
    public function hush(): void;
}
