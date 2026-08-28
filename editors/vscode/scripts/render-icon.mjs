// 构建 SVG 源文件（../../resources/icon.svg）为 VS Code 扩展所需的 PNG 图标。
// 生成物 dist/icon.png 不入库，由 build 脚本在每次编译/打包时重新生成。
import { Resvg } from '@resvg/resvg-js'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const extensionRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const svgPath = resolve(extensionRoot, '../../resources/icon.svg')
const outPath = resolve(extensionRoot, 'dist/icon.png')

const size = Number(process.argv[2] ?? 128)
const resvg = new Resvg(readFileSync(svgPath, 'utf8'), {
  fitTo: { mode: 'width', value: size },
})
mkdirSync(dirname(outPath), { recursive: true })
writeFileSync(outPath, resvg.render().asPng())
console.log(`rendered ${svgPath} -> ${outPath} (${size}x${size})`)
