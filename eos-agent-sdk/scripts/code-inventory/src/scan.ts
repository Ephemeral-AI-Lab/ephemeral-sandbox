import * as fs from "node:fs/promises";
import * as path from "node:path";

import ts from "typescript";

import {
  moduleKind,
  moduleTags,
  packageTags,
  symbolTags,
} from "./tags.js";
import type {
  ExportEdge,
  FieldInventory,
  ImportEdge,
  MethodInventory,
  ModuleInventory,
  ModuleStats,
  PackageInventory,
  PackageJsonInventory,
  PackageStats,
  Relation,
  SymbolInventory,
  SymbolKind,
  TsConfigInventory,
  VariantInventory,
  WorkspaceInventory,
  WorkspaceStats,
} from "./types.js";

interface ScannedWorkspace {
  packages: PackageInventory[];
  moduleByPath: Map<string, string>;
  moduleById: Map<string, ModuleInventory>;
}

export async function scanWorkspace(root: string): Promise<WorkspaceInventory> {
  const packageManager = await readPackageManager(root);
  const tsconfig = await readTsConfig(root);
  const groups = await moduleGroups(root);
  const packages = await Promise.all(
    groups.map((group) => scanGroup(root, group)),
  );
  packages.sort((left, right) => left.name.localeCompare(right.name));

  const scanned = indexWorkspace(packages);
  const relations = buildRelations(scanned);
  const packagesWithRelationCounts = packages.map((pkg) => ({
    ...pkg,
    stats: {
      ...pkg.stats,
      relations: relations.filter((relation) =>
        relation.from === pkg.name || relation.from.startsWith(`${pkg.name}/`),
      ).length,
    },
  }));
  const stats = workspaceStats(packagesWithRelationCounts, relations.length);

  return {
    workspace: "eos-agent-sdk",
    schemaVersion: 1,
    generatedAt: new Date().toISOString(),
    packageManager,
    tsconfig,
    packages: packagesWithRelationCounts,
    relations,
    stats,
  };
}

async function scanGroup(
  workspaceRoot: string,
  group: string,
): Promise<PackageInventory> {
  const dirs: string[] = [];
  for (const top of ["src", "tests", "e2e"]) {
    const dir = path.join(workspaceRoot, top, group);
    if (await exists(dir)) {
      dirs.push(dir);
    }
  }
  const files = (await Promise.all(dirs.map(collectTsFiles))).flat().sort();
  const modules = await Promise.all(
    files.map((file) => scanModule(workspaceRoot, group, file)),
  );
  modules.sort((left, right) => left.path.localeCompare(right.path));
  const sourceModuleCount = modules.filter((module) =>
    module.kind === "source" || module.kind === "entrypoint",
  ).length;

  return {
    id: group,
    name: group,
    path: `src/${group}`,
    packageJson: groupManifest(modules),
    tags: packageTags(group, sourceModuleCount),
    modules,
    stats: packageStats(modules, sourceModuleCount),
  };
}

/** Synthesized manifest: the single-package layout has no per-group
    package.json, so dependencies are derived from source imports. */
function groupManifest(modules: readonly ModuleInventory[]): PackageJsonInventory {
  const dependencies = new Set<string>();
  for (const module of modules) {
    if (module.kind === "test" || module.kind === "test-support") {
      continue;
    }
    for (const edge of module.imports) {
      const dependency = importedGroup(module, edge.source);
      if (dependency !== undefined && dependency !== module.packageName) {
        dependencies.add(dependency);
      }
    }
  }
  return {
    private: true,
    type: "module",
    exports: ["."],
    dependencies: [...dependencies].sort(),
    devDependencies: [],
  };
}

function importedGroup(module: ModuleInventory, source: string): string | undefined {
  if (source.startsWith("node:")) {
    return undefined;
  }
  if (source.startsWith(".")) {
    const resolved = path.posix.normalize(
      path.posix.join(path.posix.dirname(module.path), source),
    );
    const [top, group] = resolved.split("/");
    return top === "src" || top === "tests" || top === "e2e" ? group : undefined;
  }
  return scopedPackageName(source) ?? source.split("/")[0];
}

async function scanModule(
  workspaceRoot: string,
  packageName: string,
  file: string,
): Promise<ModuleInventory> {
  const source = await fs.readFile(file, "utf8");
  const sourceFile = ts.createSourceFile(
    file,
    source,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TS,
  );
  const modulePath = relativePath(workspaceRoot, file);
  const kind = moduleKind(modulePath);
  const imports = collectImports(sourceFile);
  const exports = collectExports(sourceFile);
  const localExportNames = new Set(
    exports
      .filter((edge) => edge.target === undefined)
      .flatMap((edge) => edge.exported),
  );
  const symbols = collectSymbols(
    sourceFile,
    packageName,
    modulePath,
    kind,
    imports.map((edge) => edge.source),
    localExportNames,
  );

  return {
    id: moduleId(packageName, modulePath),
    packageName,
    path: modulePath,
    kind,
    tags: moduleTags(modulePath, kind),
    imports,
    exports,
    symbols,
    stats: moduleStats(symbols),
  };
}

function collectImports(sourceFile: ts.SourceFile): ImportEdge[] {
  const imports: ImportEdge[] = [];
  for (const statement of sourceFile.statements) {
    if (!ts.isImportDeclaration(statement)) {
      continue;
    }
    if (!ts.isStringLiteral(statement.moduleSpecifier)) {
      continue;
    }
    const importClause = statement.importClause;
    const imported = importClause === undefined
      ? ["<side-effect>"]
      : importNames(importClause);
    imports.push({
      source: statement.moduleSpecifier.text,
      imported,
      typeOnly: importClause?.phaseModifier === ts.SyntaxKind.TypeKeyword,
      line: lineOf(sourceFile, statement),
    });
  }
  return imports;
}

function importNames(importClause: ts.ImportClause): string[] {
  const names: string[] = [];
  if (importClause.name !== undefined) {
    names.push(importClause.name.text);
  }
  const bindings = importClause.namedBindings;
  if (bindings === undefined) {
    return names;
  }
  if (ts.isNamespaceImport(bindings)) {
    names.push(`* as ${bindings.name.text}`);
    return names;
  }
  for (const element of bindings.elements) {
    const prefix = element.isTypeOnly ? "type " : "";
    const property = element.propertyName?.text;
    names.push(
      property === undefined
        ? `${prefix}${element.name.text}`
        : `${prefix}${property} as ${element.name.text}`,
    );
  }
  return names;
}

function collectExports(sourceFile: ts.SourceFile): ExportEdge[] {
  const exports: ExportEdge[] = [];
  for (const statement of sourceFile.statements) {
    if (ts.isExportDeclaration(statement)) {
      const exported = exportNames(statement.exportClause);
      exports.push({
        target: exportTarget(statement),
        exported,
        typeOnly: statement.isTypeOnly,
        line: lineOf(sourceFile, statement),
      });
    } else if (ts.isExportAssignment(statement)) {
      exports.push({
        exported: ["default"],
        typeOnly: false,
        line: lineOf(sourceFile, statement),
      });
    }
  }
  return exports;
}

function exportTarget(statement: ts.ExportDeclaration): string | undefined {
  const moduleSpecifier = statement.moduleSpecifier;
  if (moduleSpecifier === undefined || !ts.isStringLiteral(moduleSpecifier)) {
    return undefined;
  }
  return moduleSpecifier.text;
}

function exportNames(
  exportClause: ts.NamedExportBindings | undefined,
): string[] {
  if (exportClause === undefined) {
    return ["*"];
  }
  if (ts.isNamespaceExport(exportClause)) {
    return [`* as ${exportClause.name.text}`];
  }
  return exportClause.elements.map((element) => {
    const prefix = element.isTypeOnly ? "type " : "";
    const property = element.propertyName?.text;
    return property === undefined
      ? `${prefix}${element.name.text}`
      : `${prefix}${property} as ${element.name.text}`;
  });
}

function collectSymbols(
  sourceFile: ts.SourceFile,
  packageName: string,
  modulePath: string,
  kind: ModuleInventory["kind"],
  importSources: readonly string[],
  localExportNames: ReadonlySet<string>,
): SymbolInventory[] {
  const symbols: SymbolInventory[] = [];
  for (const statement of sourceFile.statements) {
    const symbol = symbolFromStatement(
      sourceFile,
      statement,
      packageName,
      modulePath,
      kind,
      importSources,
      localExportNames,
    );
    if (Array.isArray(symbol)) {
      symbols.push(...symbol);
    } else if (symbol !== undefined) {
      symbols.push(symbol);
    }
  }
  symbols.sort((left, right) => left.line - right.line || left.name.localeCompare(right.name));
  return symbols;
}

function symbolFromStatement(
  sourceFile: ts.SourceFile,
  statement: ts.Statement,
  packageName: string,
  modulePath: string,
  moduleKindValue: ModuleInventory["kind"],
  importSources: readonly string[],
  localExportNames: ReadonlySet<string>,
): SymbolInventory | SymbolInventory[] | undefined {
  if (ts.isClassDeclaration(statement) && statement.name !== undefined) {
    return classSymbol(
      sourceFile,
      statement,
      packageName,
      modulePath,
      moduleKindValue,
      importSources,
      localExportNames,
    );
  }
  if (ts.isInterfaceDeclaration(statement)) {
    return interfaceSymbol(
      sourceFile,
      statement,
      packageName,
      modulePath,
      moduleKindValue,
      importSources,
      localExportNames,
    );
  }
  if (ts.isTypeAliasDeclaration(statement)) {
    return typeSymbol(
      sourceFile,
      statement,
      packageName,
      modulePath,
      moduleKindValue,
      importSources,
      localExportNames,
    );
  }
  if (ts.isEnumDeclaration(statement)) {
    return enumSymbol(
      sourceFile,
      statement,
      packageName,
      modulePath,
      moduleKindValue,
      importSources,
      localExportNames,
    );
  }
  if (ts.isFunctionDeclaration(statement) && statement.name !== undefined) {
    return functionSymbol(
      sourceFile,
      statement,
      packageName,
      modulePath,
      moduleKindValue,
      importSources,
      localExportNames,
    );
  }
  if (ts.isVariableStatement(statement)) {
    return variableSymbols(
      sourceFile,
      statement,
      packageName,
      modulePath,
      moduleKindValue,
      importSources,
      localExportNames,
    );
  }
  return undefined;
}

function classSymbol(
  sourceFile: ts.SourceFile,
  node: ts.ClassDeclaration,
  packageName: string,
  modulePath: string,
  moduleKindValue: ModuleInventory["kind"],
  importSources: readonly string[],
  localExportNames: ReadonlySet<string>,
): SymbolInventory {
  const name = node.name?.text ?? "anonymous";
  const exported = isExported(node) || localExportNames.has(name);
  const heritage = heritageNames(node.heritageClauses);
  const fields = classFields(sourceFile, node);
  const methods = classMethods(sourceFile, node);
  return makeSymbol({
    sourceFile,
    node,
    packageName,
    modulePath,
    moduleKindValue,
    importSources,
    name,
    kind: "class",
    exported,
    signature: declarationHeader(sourceFile, node),
    fields,
    methods,
    variants: [],
    extendsNames: heritage.extendsNames,
    implementsNames: heritage.implementsNames,
    schemaKinds: [],
  });
}

function interfaceSymbol(
  sourceFile: ts.SourceFile,
  node: ts.InterfaceDeclaration,
  packageName: string,
  modulePath: string,
  moduleKindValue: ModuleInventory["kind"],
  importSources: readonly string[],
  localExportNames: ReadonlySet<string>,
): SymbolInventory {
  const name = node.name.text;
  const exported = isExported(node) || localExportNames.has(name);
  return makeSymbol({
    sourceFile,
    node,
    packageName,
    modulePath,
    moduleKindValue,
    importSources,
    name,
    kind: "interface",
    exported,
    signature: declarationHeader(sourceFile, node),
    fields: interfaceFields(sourceFile, node.members),
    methods: interfaceMethods(sourceFile, node.members),
    variants: [],
    extendsNames: node.heritageClauses?.flatMap((clause) =>
      clause.types.map((type) => type.expression.getText(sourceFile)),
    ) ?? [],
    implementsNames: [],
    schemaKinds: [],
  });
}

function typeSymbol(
  sourceFile: ts.SourceFile,
  node: ts.TypeAliasDeclaration,
  packageName: string,
  modulePath: string,
  moduleKindValue: ModuleInventory["kind"],
  importSources: readonly string[],
  localExportNames: ReadonlySet<string>,
): SymbolInventory {
  const name = node.name.text;
  const exported = isExported(node) || localExportNames.has(name);
  const inferred = node.type.getText(sourceFile).includes("z.infer") ? ["type:inferred"] : [];
  return makeSymbol({
    sourceFile,
    node,
    packageName,
    modulePath,
    moduleKindValue,
    importSources,
    name,
    kind: "type",
    exported,
    signature: declarationHeader(sourceFile, node),
    fields: typeFields(sourceFile, node.type),
    methods: [],
    variants: typeVariants(sourceFile, node.type),
    extendsNames: [],
    implementsNames: [],
    schemaKinds: inferred,
  });
}

function enumSymbol(
  sourceFile: ts.SourceFile,
  node: ts.EnumDeclaration,
  packageName: string,
  modulePath: string,
  moduleKindValue: ModuleInventory["kind"],
  importSources: readonly string[],
  localExportNames: ReadonlySet<string>,
): SymbolInventory {
  const name = node.name.text;
  const exported = isExported(node) || localExportNames.has(name);
  return makeSymbol({
    sourceFile,
    node,
    packageName,
    modulePath,
    moduleKindValue,
    importSources,
    name,
    kind: "enum",
    exported,
    signature: declarationHeader(sourceFile, node),
    fields: [],
    methods: [],
    variants: node.members.map((member) => ({
      name: nodeName(member.name),
      value: member.initializer?.getText(sourceFile),
    })),
    extendsNames: [],
    implementsNames: [],
    schemaKinds: [],
  });
}

function functionSymbol(
  sourceFile: ts.SourceFile,
  node: ts.FunctionDeclaration,
  packageName: string,
  modulePath: string,
  moduleKindValue: ModuleInventory["kind"],
  importSources: readonly string[],
  localExportNames: ReadonlySet<string>,
): SymbolInventory {
  const name = node.name?.text ?? "anonymous";
  const exported = isExported(node) || localExportNames.has(name);
  return makeSymbol({
    sourceFile,
    node,
    packageName,
    modulePath,
    moduleKindValue,
    importSources,
    name,
    kind: "function",
    exported,
    signature: functionSignature(sourceFile, node),
    fields: [],
    methods: [],
    variants: [],
    extendsNames: [],
    implementsNames: [],
    schemaKinds: [],
  });
}

function variableSymbols(
  sourceFile: ts.SourceFile,
  node: ts.VariableStatement,
  packageName: string,
  modulePath: string,
  moduleKindValue: ModuleInventory["kind"],
  importSources: readonly string[],
  localExportNames: ReadonlySet<string>,
): SymbolInventory[] {
  const exportedByStatement = isExported(node);
  return node.declarationList.declarations
    .filter((declaration): declaration is ts.VariableDeclaration & { name: ts.Identifier } =>
      ts.isIdentifier(declaration.name),
    )
    .map((declaration) => {
      const name = declaration.name.text;
      const exported = exportedByStatement || localExportNames.has(name);
      const schemaKinds = zodSchemaKinds(sourceFile, declaration.initializer);
      const symbolKind: SymbolKind = schemaKinds.length > 0 ? "schema" : "const";
      return makeSymbol({
        sourceFile,
        node: declaration,
        packageName,
        modulePath,
        moduleKindValue,
        importSources,
        name,
        kind: symbolKind,
        exported,
        signature: variableSignature(sourceFile, node, declaration),
        fields: zodSchemaFields(sourceFile, declaration.initializer),
        methods: [],
        variants: zodSchemaVariants(sourceFile, declaration.initializer),
        extendsNames: [],
        implementsNames: [],
        schemaKinds,
      });
    });
}

interface MakeSymbolInput {
  sourceFile: ts.SourceFile;
  node: ts.Node;
  packageName: string;
  modulePath: string;
  moduleKindValue: ModuleInventory["kind"];
  importSources: readonly string[];
  name: string;
  kind: SymbolKind;
  exported: boolean;
  signature: string;
  fields: FieldInventory[];
  methods: MethodInventory[];
  variants: VariantInventory[];
  extendsNames: string[];
  implementsNames: string[];
  schemaKinds: string[];
}

function makeSymbol(input: MakeSymbolInput): SymbolInventory {
  const id = symbolId(input.packageName, input.modulePath, input.name);
  const symbol: SymbolInventory = {
    id,
    name: input.name,
    kind: input.kind,
    exported: input.exported,
    visibility: input.exported ? "public" : "internal",
    signature: input.signature,
    docs: docsForNode(input.sourceFile, input.node),
    file: input.modulePath,
    line: lineOf(input.sourceFile, input.node),
    fields: input.fields,
    methods: input.methods,
    variants: input.variants,
    extends: input.extendsNames,
    implements: input.implementsNames,
    tags: [],
  };
  return {
    ...symbol,
    tags: symbolTags({
      packageName: input.packageName,
      modulePath: input.modulePath,
      moduleKind: input.moduleKindValue,
      name: input.name,
      kind: input.kind,
      exported: input.exported,
      signature: input.signature,
      fields: input.fields,
      extends: input.extendsNames,
      implements: input.implementsNames,
      importSources: input.importSources,
      schemaKinds: input.schemaKinds,
    }),
  };
}

function buildRelations(scanned: ScannedWorkspace): Relation[] {
  const relations: Relation[] = [];
  const packageNames = new Set(scanned.packages.map((pkg) => pkg.name));

  for (const pkg of scanned.packages) {
    for (const dependency of pkg.packageJson.dependencies) {
      relations.push({
        from: pkg.name,
        to: dependency,
        kind: "package-depends-on",
      });
    }
  }

  for (const module of scanned.moduleById.values()) {
    for (const edge of module.imports) {
      const target = resolveModuleTarget(module, edge.source, scanned, packageNames);
      relations.push({
        from: module.id,
        to: target,
        kind: "imports",
        typeOnly: edge.typeOnly,
        file: module.path,
        line: edge.line,
      });
      if (module.kind === "test" || module.kind === "test-support") {
        relations.push({
          from: module.id,
          to: target,
          kind: "tests",
          typeOnly: edge.typeOnly,
          file: module.path,
          line: edge.line,
        });
      }
    }

    for (const edge of module.exports) {
      const target = edge.target === undefined
        ? module.id
        : resolveModuleTarget(module, edge.target, scanned, packageNames);
      relations.push({
        from: module.id,
        to: target,
        kind: edge.target === undefined ? "exports" : "reexports",
        typeOnly: edge.typeOnly,
        file: module.path,
        line: edge.line,
      });
    }

    for (const symbol of module.symbols) {
      for (const target of symbol.extends) {
        relations.push({
          from: symbol.id,
          to: target,
          kind: "extends",
          file: symbol.file,
          line: symbol.line,
        });
      }
      for (const target of symbol.implements) {
        relations.push({
          from: symbol.id,
          to: target,
          kind: "implements",
          file: symbol.file,
          line: symbol.line,
        });
      }
      for (const schemaName of referencedSchemaNames(symbol.signature)) {
        const target = `${module.id}#${schemaName}`;
        if (target !== symbol.id) {
          relations.push({
            from: symbol.id,
            to: target,
            kind: "uses-schema",
            file: symbol.file,
            line: symbol.line,
          });
        }
      }
      const inferredSchema = inferredSchemaName(symbol.signature);
      if (inferredSchema !== undefined) {
        relations.push({
          from: `${module.id}#${inferredSchema}`,
          to: symbol.id,
          kind: "schema-infers-type",
          file: symbol.file,
          line: symbol.line,
        });
      }
    }
  }

  return dedupeRelations(relations).sort((left, right) =>
    left.kind.localeCompare(right.kind)
    || left.from.localeCompare(right.from)
    || left.to.localeCompare(right.to),
  );
}

function resolveModuleTarget(
  fromModule: ModuleInventory,
  source: string,
  scanned: ScannedWorkspace,
  packageNames: ReadonlySet<string>,
): string {
  if (source.startsWith(".")) {
    const fromPath = path.posix.dirname(fromModule.path);
    const withoutJs = source.endsWith(".js") ? source.slice(0, -3) : source;
    const base = path.posix.normalize(path.posix.join(fromPath, withoutJs));
    const candidates = [`${base}.ts`, `${base}/index.ts`];
    for (const candidate of candidates) {
      const found = scanned.moduleByPath.get(candidate);
      if (found !== undefined) {
        return found;
      }
    }
    return `${fromModule.packageName}/${withoutJs}`;
  }

  if (packageNames.has(source)) {
    return source;
  }
  const scopedPackage = scopedPackageName(source);
  if (scopedPackage !== undefined && packageNames.has(scopedPackage)) {
    return scopedPackage;
  }
  return source.split("/")[0];
}

function indexWorkspace(packages: readonly PackageInventory[]): ScannedWorkspace {
  const moduleByPath = new Map<string, string>();
  const moduleById = new Map<string, ModuleInventory>();
  for (const pkg of packages) {
    for (const module of pkg.modules) {
      moduleByPath.set(module.path, module.id);
      moduleById.set(module.id, module);
    }
  }
  return { packages: [...packages], moduleByPath, moduleById };
}

async function moduleGroups(root: string): Promise<string[]> {
  const entries = await fs.readdir(path.join(root, "src"), { withFileTypes: true });
  return entries
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();
}

async function collectTsFiles(root: string): Promise<string[]> {
  const out: string[] = [];
  await walk(root, out);
  return out.sort();
}

async function walk(dir: string, out: string[]): Promise<void> {
  const entries = await fs.readdir(dir, { withFileTypes: true });
  for (const entry of entries) {
    if (entry.name === "node_modules" || entry.name === "dist") {
      continue;
    }
    const next = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      await walk(next, out);
    } else if (entry.isFile() && next.endsWith(".ts")) {
      out.push(next);
    }
  }
}

async function readPackageManager(root: string): Promise<string | undefined> {
  const record = await readJsonRecord(path.join(root, "package.json"));
  return optionalString(record, "packageManager");
}

async function readTsConfig(root: string): Promise<TsConfigInventory> {
  const record = await readJsonRecord(path.join(root, "tsconfig.json"));
  const compilerOptions = optionalRecord(record, "compilerOptions");
  return {
    module: optionalString(compilerOptions, "module"),
    moduleResolution: optionalString(compilerOptions, "moduleResolution"),
    target: optionalString(compilerOptions, "target"),
    strict: optionalBoolean(compilerOptions, "strict"),
  };
}

async function readJsonRecord(file: string): Promise<Record<string, unknown>> {
  const text = await fs.readFile(file, "utf8");
  const parsed: unknown = JSON.parse(text);
  if (!isRecord(parsed)) {
    throw new Error(`expected object JSON in ${file}`);
  }
  return parsed;
}

function optionalRecord(
  record: Record<string, unknown>,
  key: string,
): Record<string, unknown> {
  const value = record[key];
  return isRecord(value) ? value : {};
}

function optionalString(
  record: Record<string, unknown>,
  key: string,
): string | undefined {
  const value = record[key];
  return typeof value === "string" ? value : undefined;
}

function optionalBoolean(
  record: Record<string, unknown>,
  key: string,
): boolean | undefined {
  const value = record[key];
  return typeof value === "boolean" ? value : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

async function exists(file: string): Promise<boolean> {
  try {
    await fs.access(file);
    return true;
  } catch {
    return false;
  }
}

function isExported(node: ts.Node): boolean {
  return hasModifier(node, ts.SyntaxKind.ExportKeyword);
}

function hasModifier(node: ts.Node, kind: ts.SyntaxKind): boolean {
  if (!ts.canHaveModifiers(node)) {
    return false;
  }
  return ts.getModifiers(node)?.some((modifier) => modifier.kind === kind) ?? false;
}

function heritageNames(clauses: ts.NodeArray<ts.HeritageClause> | undefined): {
  extendsNames: string[];
  implementsNames: string[];
} {
  const extendsNames: string[] = [];
  const implementsNames: string[] = [];
  for (const clause of clauses ?? []) {
    const names = clause.types.map((type) => type.expression.getText());
    if (clause.token === ts.SyntaxKind.ExtendsKeyword) {
      extendsNames.push(...names);
    } else {
      implementsNames.push(...names);
    }
  }
  return { extendsNames, implementsNames };
}

function classFields(
  sourceFile: ts.SourceFile,
  node: ts.ClassDeclaration,
): FieldInventory[] {
  const fields: FieldInventory[] = [];
  for (const member of node.members) {
    if (ts.isPropertyDeclaration(member)) {
      fields.push({
        name: nodeName(member.name),
        optional: member.questionToken !== undefined,
        readonly: hasModifier(member, ts.SyntaxKind.ReadonlyKeyword),
        ty: member.type?.getText(sourceFile) ?? initializerType(sourceFile, member.initializer),
      });
    }
    if (ts.isConstructorDeclaration(member)) {
      for (const parameter of member.parameters) {
        if (parameterProperty(parameter)) {
          fields.push({
            name: nodeName(parameter.name),
            optional: parameter.questionToken !== undefined,
            readonly: hasModifier(parameter, ts.SyntaxKind.ReadonlyKeyword),
            ty: parameter.type?.getText(sourceFile) ?? "unknown",
          });
        }
      }
    }
  }
  return fields;
}

function parameterProperty(parameter: ts.ParameterDeclaration): boolean {
  return [
    ts.SyntaxKind.PublicKeyword,
    ts.SyntaxKind.PrivateKeyword,
    ts.SyntaxKind.ProtectedKeyword,
    ts.SyntaxKind.ReadonlyKeyword,
  ].some((kind) => hasModifier(parameter, kind));
}

function classMethods(
  sourceFile: ts.SourceFile,
  node: ts.ClassDeclaration,
): MethodInventory[] {
  return node.members
    .filter(ts.isMethodDeclaration)
    .map((method) => ({
      name: nodeName(method.name),
      signature: methodSignature(sourceFile, method),
      async: hasModifier(method, ts.SyntaxKind.AsyncKeyword),
      static: hasModifier(method, ts.SyntaxKind.StaticKeyword),
      line: lineOf(sourceFile, method),
    }));
}

function interfaceFields(
  sourceFile: ts.SourceFile,
  members: ts.NodeArray<ts.TypeElement>,
): FieldInventory[] {
  return members
    .filter(ts.isPropertySignature)
    .map((member) => ({
      name: nodeName(member.name),
      optional: member.questionToken !== undefined,
      readonly: hasModifier(member, ts.SyntaxKind.ReadonlyKeyword),
      ty: member.type?.getText(sourceFile) ?? "unknown",
    }));
}

function interfaceMethods(
  sourceFile: ts.SourceFile,
  members: ts.NodeArray<ts.TypeElement>,
): MethodInventory[] {
  return members
    .filter(ts.isMethodSignature)
    .map((method) => ({
      name: nodeName(method.name),
      signature: method.getText(sourceFile),
      async: false,
      static: false,
      line: lineOf(sourceFile, method),
    }));
}

function typeFields(
  sourceFile: ts.SourceFile,
  node: ts.TypeNode,
): FieldInventory[] {
  if (!ts.isTypeLiteralNode(node)) {
    return [];
  }
  return interfaceFields(sourceFile, node.members);
}

function typeVariants(
  sourceFile: ts.SourceFile,
  node: ts.TypeNode,
): VariantInventory[] {
  if (!ts.isUnionTypeNode(node)) {
    return [];
  }
  return node.types.map((type) => ({
    name: compact(type.getText(sourceFile), 80),
  }));
}

function zodSchemaKinds(
  sourceFile: ts.SourceFile,
  initializer: ts.Expression | undefined,
): string[] {
  if (initializer === undefined) {
    return [];
  }
  const text = initializer.getText(sourceFile);
  const tags = new Set<string>();
  if (text.includes("z.object(")) {
    tags.add("schema:object");
  }
  if (text.includes("z.enum(")) {
    tags.add("schema:enum");
  }
  if (text.includes("z.union(")) {
    tags.add("schema:union");
  }
  if (text.includes("z.discriminatedUnion(")) {
    tags.add("schema:discriminated-union");
  }
  if (text.includes(".brand<")) {
    tags.add("schema:brand");
  }
  return [...tags].sort();
}

function zodSchemaFields(
  sourceFile: ts.SourceFile,
  initializer: ts.Expression | undefined,
): FieldInventory[] {
  const objectCall = findZodCall(initializer, "object");
  if (objectCall === undefined) {
    return [];
  }
  const firstArg = objectCall.arguments.at(0);
  if (firstArg === undefined || !ts.isObjectLiteralExpression(firstArg)) {
    return [];
  }
  return firstArg.properties.flatMap((property) => {
    if (!ts.isPropertyAssignment(property)) {
      return [];
    }
    return [{
      name: nodeName(property.name),
      optional: property.initializer.getText(sourceFile).includes(".optional("),
      readonly: false,
      ty: compact(property.initializer.getText(sourceFile), 160),
    }];
  });
}

function zodSchemaVariants(
  sourceFile: ts.SourceFile,
  initializer: ts.Expression | undefined,
): VariantInventory[] {
  const enumCall = findZodCall(initializer, "enum");
  if (enumCall !== undefined) {
    const firstArg = enumCall.arguments.at(0);
    if (firstArg !== undefined && ts.isArrayLiteralExpression(firstArg)) {
      return firstArg.elements.map((element) => ({
        name: literalName(element, sourceFile),
      }));
    }
  }

  const discriminatedUnionCall = findZodCall(initializer, "discriminatedUnion");
  if (discriminatedUnionCall === undefined) {
    return [];
  }
  const discriminatorArg = discriminatedUnionCall.arguments.at(0);
  const variantsArg = discriminatedUnionCall.arguments.at(1);
  if (
    discriminatorArg === undefined ||
    variantsArg === undefined ||
    !ts.isStringLiteral(discriminatorArg) ||
    !ts.isArrayLiteralExpression(variantsArg)
  ) {
    return [];
  }
  return variantsArg.elements.flatMap((element) =>
    discriminatedObjectVariant(sourceFile, element, discriminatorArg.text),
  );
}

function discriminatedObjectVariant(
  sourceFile: ts.SourceFile,
  element: ts.Expression,
  discriminator: string,
): VariantInventory[] {
  const objectCall = findZodCall(element, "object");
  const firstArg = objectCall?.arguments.at(0);
  if (firstArg === undefined || !ts.isObjectLiteralExpression(firstArg)) {
    return [];
  }
  for (const property of firstArg.properties) {
    if (!ts.isPropertyAssignment(property) || nodeName(property.name) !== discriminator) {
      continue;
    }
    const literalCall = findZodCall(property.initializer, "literal");
    const literalArg = literalCall?.arguments.at(0);
    if (literalArg !== undefined) {
      return [{ name: literalName(literalArg, sourceFile) }];
    }
  }
  return [];
}

function findZodCall(
  node: ts.Node | undefined,
  methodName: string,
): ts.CallExpression | undefined {
  if (node === undefined) {
    return undefined;
  }
  if (ts.isCallExpression(node) && zodMethodName(node) === methodName) {
    return node;
  }
  return node.forEachChild((child) => findZodCall(child, methodName));
}

function zodMethodName(node: ts.CallExpression): string | undefined {
  const expression = node.expression;
  if (!ts.isPropertyAccessExpression(expression)) {
    return undefined;
  }
  return expression.name.text;
}

function declarationHeader(sourceFile: ts.SourceFile, node: ts.Node): string {
  const text = node.getText(sourceFile);
  const braceIndex = text.indexOf("{");
  const semicolonIndex = text.indexOf(";");
  const end = [braceIndex, semicolonIndex]
    .filter((index) => index >= 0)
    .sort((left, right) => left - right)
    .at(0);
  return compact(end === undefined ? text : text.slice(0, end), 220);
}

function functionSignature(
  sourceFile: ts.SourceFile,
  node: ts.FunctionDeclaration,
): string {
  return declarationHeader(sourceFile, node);
}

function methodSignature(
  sourceFile: ts.SourceFile,
  node: ts.MethodDeclaration,
): string {
  return declarationHeader(sourceFile, node);
}

function variableSignature(
  sourceFile: ts.SourceFile,
  statement: ts.VariableStatement,
  declaration: ts.VariableDeclaration,
): string {
  const prefix = isExported(statement) ? "export " : "";
  const declarationKind = ts.isVariableDeclarationList(statement.declarationList)
    ? declarationListKind(statement.declarationList)
    : "const";
  const type = declaration.type?.getText(sourceFile);
  const initializer = declaration.initializer?.getText(sourceFile);
  const base = `${prefix}${declarationKind} ${declaration.name.getText(sourceFile)}${type === undefined ? "" : `: ${type}`}`;
  return compact(initializer === undefined ? base : `${base} = ${initializer}`, 220);
}

function declarationListKind(list: ts.VariableDeclarationList): "const" | "let" | "var" {
  if ((list.flags & ts.NodeFlags.Const) !== 0) {
    return "const";
  }
  if ((list.flags & ts.NodeFlags.Let) !== 0) {
    return "let";
  }
  return "var";
}

function docsForNode(sourceFile: ts.SourceFile, node: ts.Node): string | undefined {
  const text = sourceFile.getFullText();
  const comments = ts.getLeadingCommentRanges(text, node.getFullStart()) ?? [];
  const docs = comments
    .map((comment) => text.slice(comment.pos, comment.end))
    .filter((comment) => comment.startsWith("/**"))
    .map(cleanDocComment)
    .filter((comment) => comment.length > 0);
  return docs.at(-1);
}

function cleanDocComment(comment: string): string {
  return comment
    .replace(/^\/\*\*/, "")
    .replace(/\*\/$/, "")
    .split("\n")
    .map((line) => line.replace(/^\s*\*\s?/, "").trimEnd())
    .join("\n")
    .trim();
}

function initializerType(
  sourceFile: ts.SourceFile,
  initializer: ts.Expression | undefined,
): string {
  return initializer === undefined ? "unknown" : compact(initializer.getText(sourceFile), 80);
}

function nodeName(node: ts.Node): string {
  if (ts.isIdentifier(node) || ts.isStringLiteral(node) || ts.isNumericLiteral(node)) {
    return node.text;
  }
  if (ts.isPrivateIdentifier(node)) {
    return node.text;
  }
  return node.getText();
}

function literalName(node: ts.Node, sourceFile: ts.SourceFile): string {
  if (ts.isStringLiteral(node) || ts.isNumericLiteral(node)) {
    return node.text;
  }
  if (node.kind === ts.SyntaxKind.TrueKeyword) {
    return "true";
  }
  if (node.kind === ts.SyntaxKind.FalseKeyword) {
    return "false";
  }
  return compact(node.getText(sourceFile), 80);
}

function lineOf(sourceFile: ts.SourceFile, node: ts.Node): number {
  return sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1;
}

// "src/engine/agent-loop.ts" → "engine/src/agent-loop"; the group's own
// segment moves to the front, keeping the old per-package id shape.
function moduleId(packageName: string, modulePath: string): string {
  return `${packageName}/${groupLocalPath(modulePath)}`;
}

function symbolId(packageName: string, modulePath: string, name: string): string {
  return `${packageName}/${groupLocalPath(modulePath)}#${name}`;
}

function groupLocalPath(modulePath: string): string {
  const [top, , ...rest] = modulePath.split("/");
  return [top, ...rest].join("/").replace(/\.ts$/, "");
}

function relativePath(root: string, file: string): string {
  return path.relative(root, file).split(path.sep).join("/");
}

function scopedPackageName(source: string): string | undefined {
  const parts = source.split("/");
  if (source.startsWith("@") && parts.length >= 2) {
    return `${parts[0]}/${parts[1]}`;
  }
  return undefined;
}

function referencedSchemaNames(signature: string): string[] {
  const names = new Set<string>();
  for (const match of signature.matchAll(/\b([A-Z][A-Za-z0-9_]*Schema)\b/g)) {
    const [, name] = match;
    names.add(name);
  }
  return [...names].sort();
}

function inferredSchemaName(signature: string): string | undefined {
  const match = /z\.infer\s*<\s*typeof\s+([A-Z][A-Za-z0-9_]*Schema)\s*>/.exec(signature);
  return match?.[1];
}

function dedupeRelations(relations: readonly Relation[]): Relation[] {
  const seen = new Set<string>();
  const out: Relation[] = [];
  for (const relation of relations) {
    const key = [
      relation.kind,
      relation.from,
      relation.to,
      relation.typeOnly === true ? "type" : "value",
      relation.file ?? "",
      relation.line?.toString() ?? "",
    ].join("\u0000");
    if (seen.has(key)) {
      continue;
    }
    seen.add(key);
    out.push(relation);
  }
  return out;
}

function moduleStats(symbols: readonly SymbolInventory[]): ModuleStats {
  return {
    symbols: symbols.length,
    classes: symbols.filter((symbol) => symbol.kind === "class").length,
    interfaces: symbols.filter((symbol) => symbol.kind === "interface").length,
    types: symbols.filter((symbol) => symbol.kind === "type").length,
    functions: symbols.filter((symbol) => symbol.kind === "function").length,
    schemas: symbols.filter((symbol) => symbol.kind === "schema").length,
  };
}

function packageStats(
  modules: readonly ModuleInventory[],
  sourceModuleCount: number,
): PackageStats {
  const base = workspaceStats([{ modules }], 0);
  return {
    ...base,
    packages: 1,
    sourceModules: sourceModuleCount,
    testModules: modules.filter((module) =>
      module.kind === "test" || module.kind === "test-support",
    ).length,
    empty: modules.length === 0,
  };
}

function workspaceStats(
  packages: readonly ({ modules: readonly ModuleInventory[] } | PackageInventory)[],
  relationCount: number,
): WorkspaceStats {
  const modules = packages.flatMap((pkg) => pkg.modules);
  const symbols = modules.flatMap((module) => module.symbols);
  return {
    packages: packages.length,
    modules: modules.length,
    symbols: symbols.length,
    classes: symbols.filter((symbol) => symbol.kind === "class").length,
    interfaces: symbols.filter((symbol) => symbol.kind === "interface").length,
    types: symbols.filter((symbol) => symbol.kind === "type").length,
    functions: symbols.filter((symbol) => symbol.kind === "function").length,
    schemas: symbols.filter((symbol) => symbol.kind === "schema").length,
    relations: relationCount,
  };
}

function compact(text: string, maxLength: number): string {
  const normalized = text.replace(/\s+/g, " ").trim();
  if (normalized.length <= maxLength) {
    return normalized;
  }
  return `${normalized.slice(0, maxLength - 3)}...`;
}
