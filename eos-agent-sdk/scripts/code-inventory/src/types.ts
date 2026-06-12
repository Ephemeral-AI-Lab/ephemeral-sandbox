export type ModuleKind = "source" | "test" | "test-support" | "entrypoint";

export type SymbolKind =
  | "class"
  | "interface"
  | "type"
  | "function"
  | "const"
  | "enum"
  | "schema";

type RelationKind =
  | "package-depends-on"
  | "imports"
  | "exports"
  | "reexports"
  | "implements"
  | "extends"
  | "calls"
  | "uses-type"
  | "uses-schema"
  | "schema-infers-type"
  | "tests";

export interface WorkspaceInventory {
  workspace: "eos-agent-sdk";
  schemaVersion: 1;
  generatedAt: string;
  packageManager?: string;
  tsconfig: TsConfigInventory;
  packages: PackageInventory[];
  relations: Relation[];
  stats: WorkspaceStats;
}

export interface TsConfigInventory {
  module?: string;
  moduleResolution?: string;
  target?: string;
  strict?: boolean;
}

export interface PackageInventory {
  id: string;
  name: string;
  path: string;
  packageJson: PackageJsonInventory;
  tags: string[];
  modules: ModuleInventory[];
  stats: PackageStats;
}

export interface PackageJsonInventory {
  private?: boolean;
  type?: string;
  exports: string[];
  dependencies: string[];
  devDependencies: string[];
}

export interface ModuleInventory {
  id: string;
  packageName: string;
  path: string;
  kind: ModuleKind;
  tags: string[];
  imports: ImportEdge[];
  exports: ExportEdge[];
  symbols: SymbolInventory[];
  stats: ModuleStats;
}

export interface ImportEdge {
  source: string;
  imported: string[];
  typeOnly: boolean;
  line: number;
}

export interface ExportEdge {
  target?: string;
  exported: string[];
  typeOnly: boolean;
  line: number;
}

export interface SymbolInventory {
  id: string;
  name: string;
  kind: SymbolKind;
  exported: boolean;
  visibility: "public" | "internal";
  signature: string;
  docs?: string;
  file: string;
  line: number;
  fields: FieldInventory[];
  methods: MethodInventory[];
  variants: VariantInventory[];
  extends: string[];
  implements: string[];
  tags: string[];
}

export interface FieldInventory {
  name: string;
  optional: boolean;
  readonly: boolean;
  ty: string;
}

export interface MethodInventory {
  name: string;
  signature: string;
  async: boolean;
  static: boolean;
  line: number;
}

export interface VariantInventory {
  name: string;
  value?: string;
}

export interface Relation {
  from: string;
  to: string;
  kind: RelationKind;
  typeOnly?: boolean;
  file?: string;
  line?: number;
}

export interface WorkspaceStats {
  packages: number;
  modules: number;
  symbols: number;
  classes: number;
  interfaces: number;
  types: number;
  functions: number;
  schemas: number;
  relations: number;
}

export interface PackageStats extends WorkspaceStats {
  sourceModules: number;
  testModules: number;
  empty: boolean;
}

export interface ModuleStats {
  symbols: number;
  classes: number;
  interfaces: number;
  types: number;
  functions: number;
  schemas: number;
}
