<?php
namespace App\Util;

/**
 * Greets people.
 */
class Greeter
{
    private array $seen = [];

    /**
     * Say hello.
     *
     * @param string $name Who to greet.
     */
    public function hi(string $name, string $suffix = '!'): string
    {
        $this->seen[] = $name;
        return 'hi ' . $name . $suffix;
    }

    public function seen()
    {
        return $this->seen;
    }

    public static function make(): self
    {
        return new self();
    }
}
