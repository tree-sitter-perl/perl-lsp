; Composer's generated `vendor/composer/autoload_classmap.php`: one
; `'Class\\Name' => <path>` entry per class in the returned array. Composer
; spells each path as `$vendorDir . '/…'` or `$baseDir . '/…'`, or as the
; absolute string when the file is under neither. Nothing else is read.

(return_statement
  (array_creation_expression
    (array_element_initializer
      .
      (string) @classmap.class
      .
      (binary_expression
        left: (variable_name (name) @classmap.anchor)
        operator: "."
        right: (string) @classmap.path.vendor)
      .)
    (#eq? @classmap.anchor "vendorDir")))

(return_statement
  (array_creation_expression
    (array_element_initializer
      .
      (string) @classmap.class
      .
      (binary_expression
        left: (variable_name (name) @classmap.anchor)
        operator: "."
        right: (string) @classmap.path.base)
      .)
    (#eq? @classmap.anchor "baseDir")))

(return_statement
  (array_creation_expression
    (array_element_initializer
      .
      (string) @classmap.class
      .
      (string) @classmap.path.absolute
      .)))
